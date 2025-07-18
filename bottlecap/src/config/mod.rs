pub mod additional_endpoints;
pub mod apm_replace_rule;
pub mod aws;
pub mod env;
pub mod flush_strategy;
pub mod log_level;
pub mod logs_additional_endpoints;
pub mod processing_rule;
pub mod service_mapping;
pub mod trace_propagation_style;
pub mod yaml;

use datadog_trace_obfuscation::replacer::ReplaceRule;
use datadog_trace_utils::config_utils::{trace_intake_url, trace_intake_url_prefixed};

use serde::{Deserialize, Deserializer};
use serde_aux::prelude::deserialize_bool_from_anything;
use serde_json::Value;

use std::path::Path;
use std::time::Duration;
use std::{collections::HashMap, fmt};
use tracing::{debug, error};

use crate::config::{
    apm_replace_rule::deserialize_apm_replace_rules,
    env::EnvConfigSource,
    flush_strategy::FlushStrategy,
    log_level::LogLevel,
    logs_additional_endpoints::LogsAdditionalEndpoint,
    processing_rule::{deserialize_processing_rules, ProcessingRule},
    trace_propagation_style::TracePropagationStyle,
    yaml::YamlConfigSource,
};

/// Helper macro to merge Option<String> fields to String fields
///
/// Providing one field argument will merge the value from the source config field into the config
/// field.
///
/// Providing two field arguments will merge the value from the source config field into the config
/// field if the value is not empty.
#[macro_export]
macro_rules! merge_string {
    ($config:expr, $config_field:ident, $source:expr, $source_field:ident) => {
        if let Some(value) = &$source.$source_field {
            $config.$config_field.clone_from(value);
        }
    };
    ($config:expr, $source:expr, $field:ident) => {
        if let Some(value) = &$source.$field {
            $config.$field.clone_from(value);
        }
    };
}

/// Helper macro to merge Option<T> fields where T implements Clone
///
/// Providing one field argument will merge the value from the source config field into the config
/// field.
///
/// Providing two field arguments will merge the value from the source config field into the config
/// field if the value is not empty.
#[macro_export]
macro_rules! merge_option {
    ($config:expr, $config_field:ident, $source:expr, $source_field:ident) => {
        if $source.$source_field.is_some() {
            $config.$config_field.clone_from(&$source.$source_field);
        }
    };
    ($config:expr, $source:expr, $field:ident) => {
        if $source.$field.is_some() {
            $config.$field.clone_from(&$source.$field);
        }
    };
}

/// Helper macro to merge Option<T> fields to T fields when Option<T> is Some
///
/// Providing one field argument will merge the value from the source config field into the config
/// field.
///
/// Providing two field arguments will merge the value from the source config field into the config
/// field if the value is not empty.
#[macro_export]
macro_rules! merge_option_to_value {
    ($config:expr, $config_field:ident, $source:expr, $source_field:ident) => {
        if let Some(value) = &$source.$source_field {
            $config.$config_field = value.clone();
        }
    };
    ($config:expr, $source:expr, $field:ident) => {
        if let Some(value) = &$source.$field {
            $config.$field = value.clone();
        }
    };
}

/// Helper macro to merge `Vec` fields when `Vec` is not empty
///
/// Providing one field argument will merge the value from the source config field into the config
/// field.
///
/// Providing two field arguments will merge the value from the source config field into the config
/// field if the value is not empty.
#[macro_export]
macro_rules! merge_vec {
    ($config:expr, $config_field:ident, $source:expr, $source_field:ident) => {
        if !$source.$source_field.is_empty() {
            $config.$config_field.clone_from(&$source.$source_field);
        }
    };
    ($config:expr, $source:expr, $field:ident) => {
        if !$source.$field.is_empty() {
            $config.$field.clone_from(&$source.$field);
        }
    };
}

// nit: these will replace one map with the other, not merge the maps togehter, right?
/// Helper macro to merge `HashMap` fields when `HashMap` is not empty
///
/// Providing one field argument will merge the value from the source config field into the config
/// field.
///
/// Providing two field arguments will merge the value from the source config field into the config
/// field if the value is not empty.
#[macro_export]
macro_rules! merge_hashmap {
    ($config:expr, $config_field:ident, $source:expr, $source_field:ident) => {
        if !$source.$source_field.is_empty() {
            $config.$config_field.clone_from(&$source.$source_field);
        }
    };
    ($config:expr, $source:expr, $field:ident) => {
        if !$source.$field.is_empty() {
            $config.$field.clone_from(&$source.$field);
        }
    };
}

#[derive(Debug, PartialEq)]
#[allow(clippy::module_name_repetitions)]
pub enum ConfigError {
    ParseError(String),
    UnsupportedField(String),
}

#[allow(clippy::module_name_repetitions)]
pub trait ConfigSource {
    fn load(&self, config: &mut Config) -> Result<(), ConfigError>;
}

#[derive(Default)]
#[allow(clippy::module_name_repetitions)]
pub struct ConfigBuilder {
    sources: Vec<Box<dyn ConfigSource>>,
    config: Config,
}

#[allow(clippy::module_name_repetitions)]
impl ConfigBuilder {
    #[must_use]
    pub fn add_source(mut self, source: Box<dyn ConfigSource>) -> Self {
        self.sources.push(source);
        self
    }

    pub fn build(&mut self) -> Config {
        let mut failed_sources = 0;
        for source in &self.sources {
            match source.load(&mut self.config) {
                Ok(()) => (),
                Err(e) => {
                    error!("Failed to load config: {:?}", e);
                    failed_sources += 1;
                }
            }
        }

        if !self.sources.is_empty() && failed_sources == self.sources.len() {
            debug!("All sources failed to load config, using default config.");
        }

        if self.config.site.is_empty() {
            self.config.site = "datadoghq.com".to_string();
        }

        // If `proxy_https` is not set, set it from `HTTPS_PROXY` environment variable
        // if it exists
        if let Ok(https_proxy) = std::env::var("HTTPS_PROXY") {
            if self.config.proxy_https.is_none() {
                self.config.proxy_https = Some(https_proxy);
            }
        }

        // If `proxy_https` is set, check if the site is in `NO_PROXY` environment variable
        // or in the `proxy_no_proxy` config field.
        if self.config.proxy_https.is_some() {
            let site_in_no_proxy = std::env::var("NO_PROXY")
                .is_ok_and(|no_proxy| no_proxy.contains(&self.config.site))
                || self
                    .config
                    .proxy_no_proxy
                    .iter()
                    .any(|no_proxy| no_proxy.contains(&self.config.site));
            if site_in_no_proxy {
                self.config.proxy_https = None;
            }
        }

        // If extraction is not set, set it to the same as the propagation style
        if self.config.trace_propagation_style_extract.is_empty() {
            self.config
                .trace_propagation_style_extract
                .clone_from(&self.config.trace_propagation_style);
        }

        // If Logs URL is not set, set it to the default
        if self.config.logs_config_logs_dd_url.is_empty() {
            self.config.logs_config_logs_dd_url = build_fqdn_logs(self.config.site.clone());
        }

        // If APM URL is not set, set it to the default
        if self.config.apm_dd_url.is_empty() {
            self.config.apm_dd_url = trace_intake_url(self.config.site.clone().as_str());
        } else {
            // If APM URL is set, add the site to the URL
            self.config.apm_dd_url = trace_intake_url_prefixed(self.config.apm_dd_url.as_str());
        }

        self.config.clone()
    }
}

#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::module_name_repetitions)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub site: String,
    pub api_key: String,
    pub log_level: LogLevel,

    pub flush_timeout: u64,

    // Proxy
    pub proxy_https: Option<String>,
    pub proxy_no_proxy: Vec<String>,
    pub http_protocol: Option<String>,

    // Endpoints
    pub dd_url: String,
    pub url: String,
    pub additional_endpoints: HashMap<String, Vec<String>>,

    // Unified Service Tagging
    pub env: Option<String>,
    pub service: Option<String>,
    pub version: Option<String>,
    pub tags: HashMap<String, String>,

    // Logs
    pub logs_config_logs_dd_url: String,
    pub logs_config_processing_rules: Option<Vec<ProcessingRule>>,
    pub logs_config_use_compression: bool,
    pub logs_config_compression_level: i32,
    pub logs_config_additional_endpoints: Vec<LogsAdditionalEndpoint>,

    // APM
    //
    pub service_mapping: HashMap<String, String>,
    //
    pub apm_dd_url: String,
    pub apm_replace_tags: Option<Vec<ReplaceRule>>,
    pub apm_config_obfuscation_http_remove_query_string: bool,
    pub apm_config_obfuscation_http_remove_paths_with_digits: bool,
    pub apm_features: Vec<String>,
    //
    // Trace Propagation
    pub trace_propagation_style: Vec<TracePropagationStyle>,
    pub trace_propagation_style_extract: Vec<TracePropagationStyle>,
    pub trace_propagation_extract_first: bool,
    pub trace_propagation_http_baggage_enabled: bool,

    // OTLP
    //
    // - APM / Traces
    pub otlp_config_traces_enabled: bool,
    pub otlp_config_traces_span_name_as_resource_name: bool,
    pub otlp_config_traces_span_name_remappings: HashMap<String, String>,
    pub otlp_config_ignore_missing_datadog_fields: bool,
    //
    // - Receiver / HTTP
    pub otlp_config_receiver_protocols_http_endpoint: Option<String>,
    // - Unsupported Configuration
    //
    // - Receiver / GRPC
    pub otlp_config_receiver_protocols_grpc_endpoint: Option<String>,
    pub otlp_config_receiver_protocols_grpc_transport: Option<String>,
    pub otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib: Option<i32>,
    // - Metrics
    pub otlp_config_metrics_enabled: bool,
    pub otlp_config_metrics_resource_attributes_as_tags: bool,
    pub otlp_config_metrics_instrumentation_scope_metadata_as_tags: bool,
    pub otlp_config_metrics_tag_cardinality: Option<String>,
    pub otlp_config_metrics_delta_ttl: Option<i32>,
    pub otlp_config_metrics_histograms_mode: Option<String>,
    pub otlp_config_metrics_histograms_send_count_sum_metrics: bool,
    pub otlp_config_metrics_histograms_send_aggregation_metrics: bool,
    pub otlp_config_metrics_sums_cumulative_monotonic_mode: Option<String>,
    // nit: is the e in cumulative missing intentionally?
    pub otlp_config_metrics_sums_initial_cumulativ_monotonic_value: Option<String>,
    pub otlp_config_metrics_summaries_mode: Option<String>,
    // - Traces
    pub otlp_config_traces_probabilistic_sampler_sampling_percentage: Option<i32>,
    // - Logs
    pub otlp_config_logs_enabled: bool,

    // AWS Lambda
    pub api_key_secret_arn: String,
    pub kms_api_key: String,
    pub serverless_logs_enabled: bool,
    pub serverless_flush_strategy: FlushStrategy,
    pub enhanced_metrics: bool,
    pub lambda_proc_enhanced_metrics: bool,
    pub capture_lambda_payload: bool,
    pub capture_lambda_payload_max_depth: u32,
    pub serverless_appsec_enabled: bool,
    pub appsec_rules: Option<String>,
    pub appsec_waf_timeout: Duration,
    pub api_security_sample_delay: Duration,
    pub extension_version: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            site: String::default(),
            api_key: String::default(),
            log_level: LogLevel::default(),
            flush_timeout: 30,

            // Proxy
            proxy_https: None,
            proxy_no_proxy: vec![],
            http_protocol: None,

            // Endpoints
            dd_url: String::default(),
            url: String::default(),
            additional_endpoints: HashMap::new(),

            // Unified Service Tagging
            env: None,
            service: None,
            version: None,
            tags: HashMap::new(),

            // Logs
            logs_config_logs_dd_url: String::default(),
            logs_config_processing_rules: None,
            logs_config_use_compression: true,
            logs_config_compression_level: 6,
            logs_config_additional_endpoints: Vec::new(),

            // APM
            service_mapping: HashMap::new(),
            apm_dd_url: String::default(),
            apm_replace_tags: None,
            apm_config_obfuscation_http_remove_query_string: false,
            apm_config_obfuscation_http_remove_paths_with_digits: false,
            apm_features: vec![],
            trace_propagation_style: vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
            ],
            trace_propagation_style_extract: vec![],
            trace_propagation_extract_first: false,
            trace_propagation_http_baggage_enabled: false,

            // OTLP
            otlp_config_traces_enabled: true,
            otlp_config_traces_span_name_as_resource_name: false,
            otlp_config_traces_span_name_remappings: HashMap::new(),
            otlp_config_ignore_missing_datadog_fields: false,
            otlp_config_receiver_protocols_http_endpoint: None,
            otlp_config_receiver_protocols_grpc_endpoint: None,
            otlp_config_receiver_protocols_grpc_transport: None,
            otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib: None,
            otlp_config_metrics_enabled: false, // TODO(duncanista): Go Agent default is to true
            otlp_config_metrics_resource_attributes_as_tags: false,
            otlp_config_metrics_instrumentation_scope_metadata_as_tags: false,
            otlp_config_metrics_tag_cardinality: None,
            otlp_config_metrics_delta_ttl: None,
            otlp_config_metrics_histograms_mode: None,
            otlp_config_metrics_histograms_send_count_sum_metrics: false,
            otlp_config_metrics_histograms_send_aggregation_metrics: false,
            otlp_config_metrics_sums_cumulative_monotonic_mode: None,
            otlp_config_metrics_sums_initial_cumulativ_monotonic_value: None,
            otlp_config_metrics_summaries_mode: None,
            otlp_config_traces_probabilistic_sampler_sampling_percentage: None,
            otlp_config_logs_enabled: false,

            // AWS Lambda
            api_key_secret_arn: String::default(),
            kms_api_key: String::default(),
            serverless_logs_enabled: true,
            serverless_flush_strategy: FlushStrategy::Default,
            enhanced_metrics: true,
            lambda_proc_enhanced_metrics: true,
            capture_lambda_payload: false,
            capture_lambda_payload_max_depth: 10,
            serverless_appsec_enabled: false,
            appsec_rules: None,
            appsec_waf_timeout: Duration::from_millis(1),
            api_security_sample_delay: Duration::from_secs(30),
            extension_version: None,
        }
    }
}

fn log_fallback_reason(reason: &str) {
    println!("{{\"DD_EXTENSION_FALLBACK_REASON\":\"{reason}\"}}");
}

fn fallback(config: &Config) -> Result<(), ConfigError> {
    // Customer explicitly opted out of the Next Gen extension
    let opted_out = match config.extension_version.as_deref() {
        Some("compatibility") => true,
        // We want customers using the `next` to not be affected
        _ => false,
    };

    if opted_out {
        log_fallback_reason("extension_version");
        return Err(ConfigError::UnsupportedField(
            "extension_version".to_string(),
        ));
    }

    // OTLP
    let has_otlp_config = config
        .otlp_config_receiver_protocols_grpc_endpoint
        .is_some()
        || config
            .otlp_config_receiver_protocols_grpc_transport
            .is_some()
        || config
            .otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
            .is_some()
        || config.otlp_config_metrics_enabled
        || config.otlp_config_metrics_resource_attributes_as_tags
        || config.otlp_config_metrics_instrumentation_scope_metadata_as_tags
        || config.otlp_config_metrics_tag_cardinality.is_some()
        || config.otlp_config_metrics_delta_ttl.is_some()
        || config.otlp_config_metrics_histograms_mode.is_some()
        || config.otlp_config_metrics_histograms_send_count_sum_metrics
        || config.otlp_config_metrics_histograms_send_aggregation_metrics
        || config
            .otlp_config_metrics_sums_cumulative_monotonic_mode
            .is_some()
        || config
            .otlp_config_metrics_sums_initial_cumulativ_monotonic_value
            .is_some()
        || config.otlp_config_metrics_summaries_mode.is_some()
        || config
            .otlp_config_traces_probabilistic_sampler_sampling_percentage
            .is_some()
        || config.otlp_config_logs_enabled;

    if has_otlp_config {
        log_fallback_reason("otel");
        return Err(ConfigError::UnsupportedField("otel".to_string()));
    }

    Ok(())
}

#[allow(clippy::module_name_repetitions)]
pub fn get_config(config_directory: &Path) -> Result<Config, ConfigError> {
    let path: std::path::PathBuf = config_directory.join("datadog.yaml");
    let mut config_builder = ConfigBuilder::default()
        .add_source(Box::new(YamlConfigSource { path }))
        .add_source(Box::new(EnvConfigSource));

    let config = config_builder.build();

    fallback(&config)?;

    Ok(config)
}

#[inline]
#[must_use]
fn build_fqdn_logs(site: String) -> String {
    format!("https://http-intake.logs.{site}")
}

pub fn deserialize_string_or_int<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(s))
            }
        }
        Value::Number(n) => Ok(Some(n.to_string())),
        _ => Err(serde::de::Error::custom("expected a string or an integer")),
    }
}

pub fn deserialize_optional_duration_from_optional_microseconds<'de, D>(
    deserializer: D,
) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let micros: Option<u64> = Option::deserialize(deserializer)?;
    Ok(micros.map(Duration::from_micros))
}

pub fn deserialize_optional_duration_from_optional_seconds<'de, D>(
    deserializer: D,
) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let micros: Option<f64> = Option::deserialize(deserializer)?;
    Ok(micros.map(Duration::from_secs_f64))
}

pub fn deserialize_optional_bool_from_anything<'de, D>(
    deserializer: D,
) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    // First try to deserialize as Option<_> to handle null/missing values
    let opt: Option<serde_json::Value> = Option::deserialize(deserializer)?;

    match opt {
        None => Ok(None),
        Some(value) => {
            // Use your existing method by deserializing the value
            let bool_result = deserialize_bool_from_anything(value).map_err(|e| {
                serde::de::Error::custom(format!("Failed to deserialize bool: {e}"))
            })?;
            Ok(Some(bool_result))
        }
    }
}

pub fn deserialize_key_value_pairs<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct KeyValueVisitor;

    impl serde::de::Visitor<'_> for KeyValueVisitor {
        type Value = HashMap<String, String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string in format 'key1:value1,key2:value2' or 'key1:value1'")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let mut map = HashMap::new();

            for tag in value.split(',') {
                let parts = tag.split(':').collect::<Vec<&str>>();
                if parts.len() == 2 {
                    map.insert(parts[0].to_string(), parts[1].to_string());
                }
            }

            Ok(map)
        }
    }

    deserializer.deserialize_str(KeyValueVisitor)
}

pub fn deserialize_array_from_comma_separated_string<'de, D>(
    deserializer: D,
) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = String::deserialize(deserializer)?;
    Ok(s.split(',')
        .map(|feature| feature.trim().to_string())
        .filter(|feature| !feature.is_empty())
        .collect())
}

pub fn deserialize_key_value_pair_array_to_hashmap<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let array: Vec<String> = Vec::deserialize(deserializer)?;
    let mut map = HashMap::new();
    for s in array {
        let parts = s.split(':').collect::<Vec<&str>>();
        if parts.len() == 2 {
            map.insert(parts[0].to_string(), parts[1].to_string());
        }
    }
    Ok(map)
}

#[cfg(test)]
pub mod tests {
    use datadog_trace_obfuscation::replacer::parse_rules_from_string;

    use super::*;

    use crate::config::{
        flush_strategy::{FlushStrategy, PeriodicStrategy},
        log_level::LogLevel,
        trace_propagation_style::TracePropagationStyle,
    };

    #[test]
    fn test_reject_on_opted_out() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_EXTENSION_VERSION", "compatibility");
            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("extension_version".to_string())
            );
            Ok(())
        });
    }

    #[test]
    fn test_fallback_on_otel() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_ENDPOINT",
                "localhost:4138",
            );

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(config, ConfigError::UnsupportedField("otel".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_fallback_on_otel_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                otlp_config:
                  receiver:
                    protocols:
                      grpc:
                        endpoint: localhost:4138
            ",
            )?;

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(config, ConfigError::UnsupportedField("otel".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_default_logs_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config.logs_config_logs_dd_url,
                "https://http-intake.logs.datadoghq.com".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_support_pci_logs_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_LOGS_CONFIG_LOGS_DD_URL",
                "agent-http-intake-pci.logs.datadoghq.com:443",
            );

            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config.logs_config_logs_dd_url,
                "agent-http-intake-pci.logs.datadoghq.com:443".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_support_pci_traces_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_APM_DD_URL", "https://trace-pci.agent.datadoghq.com");

            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config.apm_dd_url,
                "https://trace-pci.agent.datadoghq.com/api/v0.2/traces".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_support_dd_dd_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_DD_URL", "custom_proxy:3128");

            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.dd_url, "custom_proxy:3128".to_string());
            Ok(())
        });
    }

    #[test]
    fn test_support_dd_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_URL", "custom_proxy:3128");

            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.url, "custom_proxy:3128".to_string());
            Ok(())
        });
    }

    #[test]
    fn test_dd_dd_url_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.dd_url, String::new());
            Ok(())
        });
    }

    #[test]
    fn test_precedence() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                site: datadoghq.eu,
            ",
            )?;
            jail.set_env("DD_SITE", "datad0g.com");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.site, "datad0g.com");
            Ok(())
        });
    }

    #[test]
    fn test_parse_config_file() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            // nit: does parsing an empty file actually test "parse config file"?
            jail.create_file(
                "datadog.yaml",
                r"
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.site, "datadoghq.com");
            Ok(())
        });
    }

    #[test]
    fn test_parse_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SITE", "datadoghq.eu");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.site, "datadoghq.eu");
            Ok(())
        });
    }

    #[test]
    fn test_parse_log_level() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_LOG_LEVEL", "TRACE");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.log_level, LogLevel::Trace);
            Ok(())
        });
    }

    #[test]
    fn test_parse_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    site: "datadoghq.com".to_string(),
                    trace_propagation_style_extract: vec![
                        TracePropagationStyle::Datadog,
                        TracePropagationStyle::TraceContext
                    ],
                    logs_config_logs_dd_url: "https://http-intake.logs.datadoghq.com".to_string(),
                    apm_dd_url: trace_intake_url("datadoghq.com").to_string(),
                    dd_url: String::new(), // We add the prefix in main.rs
                    ..Config::default()
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_proxy_config() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_PROXY_HTTPS", "my-proxy:3128");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.proxy_https, Some("my-proxy:3128".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_noproxy_config() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SITE", "datadoghq.eu");
            jail.set_env("DD_PROXY_HTTPS", "my-proxy:3128");
            jail.set_env(
                "NO_PROXY",
                "127.0.0.1,localhost,172.16.0.0/12,us-east-1.amazonaws.com,datadoghq.eu",
            );
            let config = get_config(Path::new("")).expect("should parse noproxy");
            assert_eq!(config.proxy_https, None);
            Ok(())
        });
    }

    #[test]
    fn test_proxy_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                proxy:
                  https: my-proxy:3128
            ",
            )?;

            let config = get_config(Path::new("")).expect("should parse weird proxy config");
            assert_eq!(config.proxy_https, Some("my-proxy:3128".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_no_proxy_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                proxy:
                  https: my-proxy:3128
                  no_proxy:
                    - datadoghq.com
            ",
            )?;

            let config = get_config(Path::new("")).expect("should parse weird proxy config");
            assert_eq!(config.proxy_https, None);
            // Assertion to ensure config.site runs before proxy
            // because we chenck that noproxy contains the site
            assert_eq!(config.site, "datadoghq.com");
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_end() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "end");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.serverless_flush_strategy, FlushStrategy::End);
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_periodically() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "periodically,100000");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config.serverless_flush_strategy,
                FlushStrategy::Periodically(PeriodicStrategy { interval: 100_000 })
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_invalid() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "invalid_strategy");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.serverless_flush_strategy, FlushStrategy::Default);
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_invalid_periodic() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_SERVERLESS_FLUSH_STRATEGY",
                "periodically,invalid_interval",
            );
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.serverless_flush_strategy, FlushStrategy::Default);
            Ok(())
        });
    }

    #[test]
    fn parse_number_or_string_env_vars() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_VERSION", "123");
            jail.set_env("DD_ENV", "123456890");
            jail.set_env("DD_SERVICE", "123456");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.version.expect("failed to parse DD_VERSION"), "123");
            assert_eq!(config.env.expect("failed to parse DD_ENV"), "123456890");
            assert_eq!(
                config.service.expect("failed to parse DD_SERVICE"),
                "123456"
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_logs_config_processing_rules_from_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_LOGS_CONFIG_PROCESSING_RULES",
                r#"[{"type":"exclude_at_match","name":"exclude","pattern":"exclude"}]"#,
            );
            jail.create_file(
                "datadog.yaml",
                r"
                extension_version: next
                logs_config:
                  processing_rules:
                    - type: exclude_at_match
                      name: exclude-me-yaml
                      pattern: exclude-me-yaml
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config.logs_config_processing_rules,
                Some(vec![ProcessingRule {
                    kind: processing_rule::Kind::ExcludeAtMatch,
                    name: "exclude".to_string(),
                    pattern: "exclude".to_string(),
                    replace_placeholder: None
                }])
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_logs_config_processing_rules_from_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                site: datadoghq.com
                logs_config:
                  processing_rules:
                    - type: exclude_at_match
                      name: exclude
                      pattern: exclude
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config.logs_config_processing_rules,
                Some(vec![ProcessingRule {
                    kind: processing_rule::Kind::ExcludeAtMatch,
                    name: "exclude".to_string(),
                    pattern: "exclude".to_string(),
                    replace_placeholder: None
                }]),
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_apm_replace_tags_from_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                site: datadoghq.com
                apm_config:
                  replace_tags:
                    - name: '*'
                      pattern: 'foo'
                      repl: 'REDACTED'
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            let rule = parse_rules_from_string(
                r#"[
                        {"name": "*", "pattern": "foo", "repl": "REDACTED"}
                    ]"#,
            )
            .expect("can't parse rules");
            assert_eq!(config.apm_replace_tags, Some(rule),);
            Ok(())
        });
    }

    #[test]
    fn test_apm_tags_env_overrides_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_APM_REPLACE_TAGS",
                r#"[{"name":"*","pattern":"foo","repl":"REDACTED-ENV"}]"#,
            );
            jail.create_file(
                "datadog.yaml",
                r"
                site: datadoghq.com
                apm_config:
                  replace_tags:
                    - name: '*'
                      pattern: 'foo'
                      repl: 'REDACTED-YAML'
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            let rule = parse_rules_from_string(
                r#"[
                        {"name": "*", "pattern": "foo", "repl": "REDACTED-ENV"}
                    ]"#,
            )
            .expect("can't parse rules");
            assert_eq!(config.apm_replace_tags, Some(rule),);
            Ok(())
        });
    }

    #[test]
    fn test_parse_apm_http_obfuscation_from_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                site: datadoghq.com
                apm_config:
                  obfuscation:
                    http:
                      remove_query_string: true
                      remove_paths_with_digits: true
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            assert!(config.apm_config_obfuscation_http_remove_query_string,);
            assert!(config.apm_config_obfuscation_http_remove_paths_with_digits,);
            Ok(())
        });
    }
    #[test]
    fn test_parse_trace_propagation_style() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_TRACE_PROPAGATION_STYLE",
                "datadog,tracecontext,b3,b3multi",
            );
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");

            let expected_styles = vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
                TracePropagationStyle::B3,
                TracePropagationStyle::B3Multi,
            ];
            assert_eq!(config.trace_propagation_style, expected_styles);
            assert_eq!(config.trace_propagation_style_extract, expected_styles);
            Ok(())
        });
    }

    #[test]
    fn test_parse_trace_propagation_style_extract() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_TRACE_PROPAGATION_STYLE_EXTRACT", "datadog");
            let config = get_config(Path::new("")).expect("should parse config");

            assert_eq!(
                config.trace_propagation_style,
                vec![
                    TracePropagationStyle::Datadog,
                    TracePropagationStyle::TraceContext,
                ]
            );
            assert_eq!(
                config.trace_propagation_style_extract,
                vec![TracePropagationStyle::Datadog]
            );
            Ok(())
        });
    }

    #[test]
    fn test_ignore_apm_replace_tags() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_APM_REPLACE_TAGS",
                r#"[{"name":"resource.name","pattern":"(.*)/(foo[:%].+)","repl":"$1/{foo}"}]"#,
            );
            let config = get_config(Path::new(""));
            assert!(config.is_ok());
            Ok(())
        });
    }

    #[test]
    fn test_parse_bool_from_anything() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_LOGS_ENABLED", "true");
            jail.set_env("DD_ENHANCED_METRICS", "1");
            jail.set_env("DD_LOGS_CONFIG_USE_COMPRESSION", "TRUE");
            jail.set_env("DD_CAPTURE_LAMBDA_PAYLOAD", "0");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.serverless_logs_enabled, true);
            assert_eq!(config.enhanced_metrics, true);
            assert_eq!(config.logs_config_use_compression, true);
            assert_eq!(config.capture_lambda_payload, false);
            Ok(())
        });
    }

    #[test]
    fn test_overrides_config_based_on_priority() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r#"
                site: us3.datadoghq.com
                api_key: "yaml-api-key"
                log_level: "debug"
            "#,
            )?;
            jail.set_env("DD_SITE", "us5.datadoghq.com");
            jail.set_env("DD_API_KEY", "env-api-key");
            jail.set_env("DD_FLUSH_TIMEOUT", "10");
            let config = get_config(Path::new("")).expect("should parse config");

            assert_eq!(config.site, "us5.datadoghq.com");
            assert_eq!(config.api_key, "env-api-key");
            assert_eq!(config.log_level, LogLevel::Debug);
            assert_eq!(config.flush_timeout, 10);
            Ok(())
        });
    }
}
