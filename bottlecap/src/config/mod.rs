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

use libdd_trace_obfuscation::replacer::ReplaceRule;
use libdd_trace_utils::config_utils::{trace_intake_url, trace_intake_url_prefixed};

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
    processing_rule::{ProcessingRule, deserialize_processing_rules},
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
        } else {
            // If Logs URL is set, ensure it is prefixed correctly
            self.config.logs_config_logs_dd_url =
                logs_intake_url(self.config.logs_config_logs_dd_url.as_str());
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

    // Timeout for the request to flush data to Datadog endpoint
    pub flush_timeout: u64,

    // Global config of compression levels.
    // It would be overridden by the setup for the individual component
    pub compression_level: i32,

    // Proxy
    pub proxy_https: Option<String>,
    pub proxy_no_proxy: Vec<String>,
    pub http_protocol: Option<String>,
    pub tls_cert_file: Option<String>,

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
    pub observability_pipelines_worker_logs_enabled: bool,
    pub observability_pipelines_worker_logs_url: String,

    // APM
    //
    pub service_mapping: HashMap<String, String>,
    //
    pub apm_dd_url: String,
    pub apm_replace_tags: Option<Vec<ReplaceRule>>,
    pub apm_config_obfuscation_http_remove_query_string: bool,
    pub apm_config_obfuscation_http_remove_paths_with_digits: bool,
    pub apm_config_compression_level: i32,
    pub apm_features: Vec<String>,
    pub apm_additional_endpoints: HashMap<String, Vec<String>>,
    pub apm_filter_tags_require: Option<Vec<String>>,
    pub apm_filter_tags_reject: Option<Vec<String>>,
    pub apm_filter_tags_regex_require: Option<Vec<String>>,
    pub apm_filter_tags_regex_reject: Option<Vec<String>>,
    //
    // Trace Propagation
    pub trace_propagation_style: Vec<TracePropagationStyle>,
    pub trace_propagation_style_extract: Vec<TracePropagationStyle>,
    pub trace_propagation_extract_first: bool,
    pub trace_propagation_http_baggage_enabled: bool,
    pub trace_aws_service_representation_enabled: bool,

    // Metrics
    pub metrics_config_compression_level: i32,
    pub statsd_metric_namespace: Option<String>,

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
    pub api_key_ssm_arn: String,
    pub serverless_logs_enabled: bool,
    pub serverless_flush_strategy: FlushStrategy,
    pub enhanced_metrics: bool,
    pub lambda_proc_enhanced_metrics: bool,
    pub capture_lambda_payload: bool,
    pub capture_lambda_payload_max_depth: u32,
    pub compute_trace_stats_on_extension: bool,
    pub span_dedup_timeout: Option<Duration>,
    pub api_key_secret_reload_interval: Option<Duration>,

    pub serverless_appsec_enabled: bool,
    pub appsec_rules: Option<String>,
    pub appsec_waf_timeout: Duration,
    pub api_security_enabled: bool,
    pub api_security_sample_delay: Duration,
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
            tls_cert_file: None,

            // Endpoints
            dd_url: String::default(),
            url: String::default(),
            additional_endpoints: HashMap::new(),

            // Unified Service Tagging
            env: None,
            service: None,
            version: None,
            tags: HashMap::new(),

            compression_level: 3,

            // Logs
            logs_config_logs_dd_url: String::default(),
            logs_config_processing_rules: None,
            logs_config_use_compression: true,
            logs_config_compression_level: 3,
            logs_config_additional_endpoints: Vec::new(),
            observability_pipelines_worker_logs_enabled: false,
            observability_pipelines_worker_logs_url: String::default(),

            // APM
            service_mapping: HashMap::new(),
            apm_dd_url: String::default(),
            apm_replace_tags: None,
            apm_config_obfuscation_http_remove_query_string: false,
            apm_config_obfuscation_http_remove_paths_with_digits: false,
            apm_config_compression_level: 3,
            apm_features: vec![],
            apm_additional_endpoints: HashMap::new(),
            apm_filter_tags_require: None,
            apm_filter_tags_reject: None,
            apm_filter_tags_regex_require: None,
            apm_filter_tags_regex_reject: None,
            trace_aws_service_representation_enabled: true,
            trace_propagation_style: vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
            ],
            trace_propagation_style_extract: vec![],
            trace_propagation_extract_first: false,
            trace_propagation_http_baggage_enabled: false,

            // Metrics
            metrics_config_compression_level: 3,
            statsd_metric_namespace: None,

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
            api_key_ssm_arn: String::default(),
            serverless_logs_enabled: true,
            serverless_flush_strategy: FlushStrategy::Default,
            enhanced_metrics: true,
            lambda_proc_enhanced_metrics: true,
            capture_lambda_payload: false,
            capture_lambda_payload_max_depth: 10,
            compute_trace_stats_on_extension: false,
            span_dedup_timeout: None,
            api_key_secret_reload_interval: None,

            serverless_appsec_enabled: false,
            appsec_rules: None,
            appsec_waf_timeout: Duration::from_millis(5),
            api_security_enabled: true,
            api_security_sample_delay: Duration::from_secs(30),
        }
    }
}

#[allow(clippy::module_name_repetitions)]
#[inline]
#[must_use]
pub fn get_config(config_directory: &Path) -> Config {
    let path: std::path::PathBuf = config_directory.join("datadog.yaml");
    ConfigBuilder::default()
        .add_source(Box::new(YamlConfigSource { path }))
        .add_source(Box::new(EnvConfigSource))
        .build()
}

#[inline]
#[must_use]
fn build_fqdn_logs(site: String) -> String {
    format!("https://http-intake.logs.{site}")
}

/// Ensures logs intake URL is prefixed with https://
#[inline]
#[must_use]
fn logs_intake_url(url: &str) -> String {
    let url = url.trim();
    if url.is_empty() {
        return url.to_string();
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return url.to_string();
    }
    format!("https://{url}")
}

pub fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(s) => Ok(Some(s)),
        other => {
            error!(
                "Failed to parse value, expected a string, got: {}, ignoring",
                other
            );
            Ok(None)
        }
    }
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
        _ => {
            error!("Failed to parse value, expected a string or an integer, ignoring");
            Ok(None)
        }
    }
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
        Some(value) => match deserialize_bool_from_anything(value) {
            Ok(bool_result) => Ok(Some(bool_result)),
            Err(e) => {
                error!("Failed to parse bool value: {}, ignoring", e);
                Ok(None)
            }
        },
    }
}

/// Parse a single "key:value" string into a (key, value) tuple
/// Returns None if the string is invalid (e.g., missing colon, empty key/value)
fn parse_key_value_tag(tag: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = tag.splitn(2, ':').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        error!(
            "Failed to parse tag '{}', expected format 'key:value', ignoring",
            tag
        );
        None
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
            for tag in value.split(&[',', ' ']) {
                if tag.is_empty() {
                    continue;
                }
                if let Some((key, val)) = parse_key_value_tag(tag) {
                    map.insert(key, val);
                }
            }

            Ok(map)
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            error!(
                "Failed to parse tags: expected string in format 'key:value', got number {}, ignoring",
                value
            );
            Ok(HashMap::new())
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            error!(
                "Failed to parse tags: expected string in format 'key:value', got number {}, ignoring",
                value
            );
            Ok(HashMap::new())
        }

        fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            error!(
                "Failed to parse tags: expected string in format 'key:value', got number {}, ignoring",
                value
            );
            Ok(HashMap::new())
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            error!(
                "Failed to parse tags: expected string in format 'key:value', got boolean {}, ignoring",
                value
            );
            Ok(HashMap::new())
        }
    }

    deserializer.deserialize_any(KeyValueVisitor)
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
        if let Some((key, val)) = parse_key_value_tag(&s) {
            map.insert(key, val);
        }
    }
    Ok(map)
}

/// Deserialize APM filter tags from space-separated "key:value" pairs, also support key-only tags
pub fn deserialize_apm_filter_tags<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;

    match opt {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => {
            let tags: Vec<String> = s
                .split_whitespace()
                .filter_map(|pair| {
                    let parts: Vec<&str> = pair.splitn(2, ':').collect();
                    if parts.len() == 2 {
                        let key = parts[0].trim();
                        let value = parts[1].trim();
                        if key.is_empty() {
                            None
                        } else if value.is_empty() {
                            Some(key.to_string())
                        } else {
                            Some(format!("{key}:{value}"))
                        }
                    } else if parts.len() == 1 {
                        let key = parts[0].trim();
                        if key.is_empty() {
                            None
                        } else {
                            Some(key.to_string())
                        }
                    } else {
                        None
                    }
                })
                .collect();

            if tags.is_empty() {
                Ok(None)
            } else {
                Ok(Some(tags))
            }
        }
    }
}

pub fn deserialize_option_lossless<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    match Option::<T>::deserialize(deserializer) {
        Ok(value) => Ok(value),
        Err(e) => {
            error!("Failed to deserialize optional value: {}, ignoring", e);
            Ok(None)
        }
    }
}

pub fn deserialize_optional_duration_from_microseconds<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Duration>, D::Error> {
    Ok(Option::<u64>::deserialize(deserializer)?.map(Duration::from_micros))
}

pub fn deserialize_optional_duration_from_seconds<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Duration>, D::Error> {
    struct DurationVisitor;
    impl serde::de::Visitor<'_> for DurationVisitor {
        type Value = Option<Duration>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "a duration in seconds (integer or float)")
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(Duration::from_secs(v)))
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
            if v < 0 {
                error!("Failed to parse duration: negative durations are not allowed, ignoring");
                return Ok(None);
            }
            self.visit_u64(u64::try_from(v).expect("positive i64 to u64 conversion never fails"))
        }
        fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
            if v < 0f64 {
                error!("Failed to parse duration: negative durations are not allowed, ignoring");
                return Ok(None);
            }
            Ok(Some(Duration::from_secs_f64(v)))
        }
    }
    deserializer.deserialize_any(DurationVisitor)
}

// Like deserialize_optional_duration_from_seconds(), but return None if the value is 0
pub fn deserialize_optional_duration_from_seconds_ignore_zero<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Duration>, D::Error> {
    let duration: Option<Duration> = deserialize_optional_duration_from_seconds(deserializer)?;
    if duration.is_some_and(|d| d.as_secs() == 0) {
        return Ok(None);
    }
    Ok(duration)
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
pub mod tests {
    use libdd_trace_obfuscation::replacer::parse_rules_from_string;

    use super::*;

    use crate::config::{
        flush_strategy::{FlushStrategy, PeriodicStrategy},
        log_level::LogLevel,
        processing_rule::ProcessingRule,
        trace_propagation_style::TracePropagationStyle,
    };

    #[test]
    fn test_default_logs_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            let config = get_config(Path::new(""));
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

            let config = get_config(Path::new(""));
            assert_eq!(
                config.logs_config_logs_dd_url,
                "https://agent-http-intake-pci.logs.datadoghq.com:443".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_logs_intake_url_adds_prefix() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_LOGS_CONFIG_LOGS_DD_URL",
                "dr-test-failover-http-intake.logs.datadoghq.com:443",
            );

            let config = get_config(Path::new(""));
            // ensure host:port URL is prefixed with https://
            assert_eq!(
                config.logs_config_logs_dd_url,
                "https://dr-test-failover-http-intake.logs.datadoghq.com:443".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_prefixed_logs_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_LOGS_CONFIG_LOGS_DD_URL",
                "https://custom-intake.logs.datadoghq.com:443",
            );

            let config = get_config(Path::new(""));
            assert_eq!(
                config.logs_config_logs_dd_url,
                "https://custom-intake.logs.datadoghq.com:443".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_support_pci_traces_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_APM_DD_URL", "https://trace-pci.agent.datadoghq.com");

            let config = get_config(Path::new(""));
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

            let config = get_config(Path::new(""));
            assert_eq!(config.dd_url, "custom_proxy:3128".to_string());
            Ok(())
        });
    }

    #[test]
    fn test_support_dd_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_URL", "custom_proxy:3128");

            let config = get_config(Path::new(""));
            assert_eq!(config.url, "custom_proxy:3128".to_string());
            Ok(())
        });
    }

    #[test]
    fn test_dd_dd_url_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
            assert_eq!(config.site, "datadoghq.com");
            Ok(())
        });
    }

    #[test]
    fn test_parse_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SITE", "datadoghq.eu");
            let config = get_config(Path::new(""));
            assert_eq!(config.site, "datadoghq.eu");
            Ok(())
        });
    }

    #[test]
    fn test_parse_log_level() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_LOG_LEVEL", "TRACE");
            let config = get_config(Path::new(""));
            assert_eq!(config.log_level, LogLevel::Trace);
            Ok(())
        });
    }

    #[test]
    fn test_parse_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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

            let config = get_config(Path::new(""));
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

            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
            assert_eq!(config.serverless_flush_strategy, FlushStrategy::End);
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_periodically() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "periodically,100000");
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
                logs_config:
                  processing_rules:
                    - type: exclude_at_match
                      name: exclude-me-yaml
                      pattern: exclude-me-yaml
            ",
            )?;
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));
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
            let config = get_config(Path::new(""));

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
            let config = get_config(Path::new(""));

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
    fn test_bad_tags() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_TAGS", 123);
            let config = get_config(Path::new(""));
            assert_eq!(config.tags, HashMap::new());
            Ok(())
        });
    }

    #[test]
    fn test_tags_comma_separated() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_TAGS", "team:serverless,env:prod,version:1.0");
            let config = get_config(Path::new(""));
            assert_eq!(config.tags.get("team"), Some(&"serverless".to_string()));
            assert_eq!(config.tags.get("env"), Some(&"prod".to_string()));
            assert_eq!(config.tags.get("version"), Some(&"1.0".to_string()));
            assert_eq!(config.tags.len(), 3);
            Ok(())
        });
    }

    #[test]
    fn test_tags_space_separated() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_TAGS", "team:serverless env:prod version:1.0");
            let config = get_config(Path::new(""));
            assert_eq!(config.tags.get("team"), Some(&"serverless".to_string()));
            assert_eq!(config.tags.get("env"), Some(&"prod".to_string()));
            assert_eq!(config.tags.get("version"), Some(&"1.0".to_string()));
            assert_eq!(config.tags.len(), 3);
            Ok(())
        });
    }

    #[test]
    fn test_tags_space_separated_with_extra_spaces() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_TAGS", "team:serverless  env:prod   version:1.0");
            let config = get_config(Path::new(""));
            assert_eq!(config.tags.get("team"), Some(&"serverless".to_string()));
            assert_eq!(config.tags.get("env"), Some(&"prod".to_string()));
            assert_eq!(config.tags.get("version"), Some(&"1.0".to_string()));
            assert_eq!(config.tags.len(), 3);
            Ok(())
        });
    }

    #[test]
    fn test_tags_mixed_separators() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_TAGS", "team:serverless,env:prod version:1.0");
            let config = get_config(Path::new(""));
            assert_eq!(config.tags.get("team"), Some(&"serverless".to_string()));
            assert_eq!(config.tags.get("env"), Some(&"prod".to_string()));
            assert_eq!(config.tags.get("version"), Some(&"1.0".to_string()));
            assert_eq!(config.tags.len(), 3);
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
            let config = get_config(Path::new(""));
            assert!(config.serverless_logs_enabled);
            assert!(config.enhanced_metrics);
            assert!(config.logs_config_use_compression);
            assert!(!config.capture_lambda_payload);
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
            let config = get_config(Path::new(""));

            assert_eq!(config.site, "us5.datadoghq.com");
            assert_eq!(config.api_key, "env-api-key");
            assert_eq!(config.log_level, LogLevel::Debug);
            assert_eq!(config.flush_timeout, 10);
            Ok(())
        });
    }

    #[test]
    fn test_parse_duration_from_microseconds() {
        #[derive(Deserialize, Debug, PartialEq, Eq)]
        struct Value {
            #[serde(default)]
            #[serde(deserialize_with = "deserialize_optional_duration_from_microseconds")]
            duration: Option<Duration>,
        }

        assert_eq!(
            serde_json::from_str::<Value>("{}").expect("failed to parse JSON"),
            Value { duration: None }
        );
        serde_json::from_str::<Value>(r#"{"duration":-1}"#)
            .expect_err("should have failed parsing");
        assert_eq!(
            serde_json::from_str::<Value>(r#"{"duration":1000000}"#).expect("failed to parse JSON"),
            Value {
                duration: Some(Duration::from_secs(1))
            }
        );
        serde_json::from_str::<Value>(r#"{"duration":-1.5}"#)
            .expect_err("should have failed parsing");
        serde_json::from_str::<Value>(r#"{"duration":1.5}"#)
            .expect_err("should have failed parsing");
    }

    #[test]
    fn test_parse_duration_from_seconds() {
        #[derive(Deserialize, Debug, PartialEq, Eq)]
        struct Value {
            #[serde(default)]
            #[serde(deserialize_with = "deserialize_optional_duration_from_seconds")]
            duration: Option<Duration>,
        }

        assert_eq!(
            serde_json::from_str::<Value>("{}").expect("failed to parse JSON"),
            Value { duration: None }
        );
        assert_eq!(
            serde_json::from_str::<Value>(r#"{"duration":-1}"#).expect("failed to parse JSON"),
            Value { duration: None }
        );
        assert_eq!(
            serde_json::from_str::<Value>(r#"{"duration":1}"#).expect("failed to parse JSON"),
            Value {
                duration: Some(Duration::from_secs(1))
            }
        );
        assert_eq!(
            serde_json::from_str::<Value>(r#"{"duration":-1.5}"#).expect("failed to parse JSON"),
            Value { duration: None }
        );
        assert_eq!(
            serde_json::from_str::<Value>(r#"{"duration":1.5}"#).expect("failed to parse JSON"),
            Value {
                duration: Some(Duration::from_millis(1500))
            }
        );
    }

    #[test]
    fn test_parse_duration_from_seconds_ignore_zero() {
        #[derive(Deserialize, Debug, PartialEq, Eq)]
        struct Value {
            #[serde(default)]
            #[serde(deserialize_with = "deserialize_optional_duration_from_seconds_ignore_zero")]
            duration: Option<Duration>,
        }

        assert_eq!(
            serde_json::from_str::<Value>(r#"{"duration":1}"#).expect("failed to parse JSON"),
            Value {
                duration: Some(Duration::from_secs(1))
            }
        );

        assert_eq!(
            serde_json::from_str::<Value>(r#"{"duration":0}"#).expect("failed to parse JSON"),
            Value { duration: None }
        );
    }

    #[test]
    fn test_deserialize_key_value_pairs_ignores_empty_keys() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct TestStruct {
            #[serde(deserialize_with = "deserialize_key_value_pairs")]
            tags: HashMap<String, String>,
        }

        let result = serde_json::from_str::<TestStruct>(r#"{"tags": ":value,valid:tag"}"#)
            .expect("failed to parse JSON");
        let mut expected = HashMap::new();
        expected.insert("valid".to_string(), "tag".to_string());
        assert_eq!(result.tags, expected);
    }

    #[test]
    fn test_deserialize_key_value_pairs_ignores_empty_values() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct TestStruct {
            #[serde(deserialize_with = "deserialize_key_value_pairs")]
            tags: HashMap<String, String>,
        }

        let result = serde_json::from_str::<TestStruct>(r#"{"tags": "key:,valid:tag"}"#)
            .expect("failed to parse JSON");
        let mut expected = HashMap::new();
        expected.insert("valid".to_string(), "tag".to_string());
        assert_eq!(result.tags, expected);
    }

    #[test]
    fn test_deserialize_key_value_pairs_with_url_values() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct TestStruct {
            #[serde(deserialize_with = "deserialize_key_value_pairs")]
            tags: HashMap<String, String>,
        }

        let result = serde_json::from_str::<TestStruct>(
            r#"{"tags": "git.repository_url:https://gitlab.ddbuild.io/DataDog/serverless-e2e-tests.git,env:prod"}"#
        )
        .expect("failed to parse JSON");
        let mut expected = HashMap::new();
        expected.insert(
            "git.repository_url".to_string(),
            "https://gitlab.ddbuild.io/DataDog/serverless-e2e-tests.git".to_string(),
        );
        expected.insert("env".to_string(), "prod".to_string());
        assert_eq!(result.tags, expected);
    }

    #[test]
    fn test_deserialize_key_value_pair_array_with_urls() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct TestStruct {
            #[serde(deserialize_with = "deserialize_key_value_pair_array_to_hashmap")]
            tags: HashMap<String, String>,
        }

        let result = serde_json::from_str::<TestStruct>(
            r#"{"tags": ["git.repository_url:https://gitlab.ddbuild.io/DataDog/serverless-e2e-tests.git", "env:prod", "version:1.2.3"]}"#
        )
        .expect("failed to parse JSON");
        let mut expected = HashMap::new();
        expected.insert(
            "git.repository_url".to_string(),
            "https://gitlab.ddbuild.io/DataDog/serverless-e2e-tests.git".to_string(),
        );
        expected.insert("env".to_string(), "prod".to_string());
        expected.insert("version".to_string(), "1.2.3".to_string());
        assert_eq!(result.tags, expected);
    }

    #[test]
    fn test_deserialize_key_value_pair_array_ignores_invalid() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct TestStruct {
            #[serde(deserialize_with = "deserialize_key_value_pair_array_to_hashmap")]
            tags: HashMap<String, String>,
        }

        let result = serde_json::from_str::<TestStruct>(
            r#"{"tags": ["valid:tag", "invalid_no_colon", "another:good:value:with:colons"]}"#,
        )
        .expect("failed to parse JSON");
        let mut expected = HashMap::new();
        expected.insert("valid".to_string(), "tag".to_string());
        expected.insert("another".to_string(), "good:value:with:colons".to_string());
        assert_eq!(result.tags, expected);
    }

    #[test]
    fn test_deserialize_key_value_pair_array_empty() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct TestStruct {
            #[serde(deserialize_with = "deserialize_key_value_pair_array_to_hashmap")]
            tags: HashMap<String, String>,
        }

        let result =
            serde_json::from_str::<TestStruct>(r#"{"tags": []}"#).expect("failed to parse JSON");
        assert_eq!(result.tags, HashMap::new());
    }
}
