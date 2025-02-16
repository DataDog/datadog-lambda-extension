pub mod flush_strategy;
pub mod log_level;
pub mod processing_rule;
pub mod service_mapping;
pub mod trace_propagation_style;

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use std::vec;

use figment::providers::{Format, Yaml};
use figment::{providers::Env, Figment};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use trace_propagation_style::{deserialize_trace_propagation_style, TracePropagationStyle};

use crate::config::{
    flush_strategy::FlushStrategy,
    log_level::{deserialize_log_level, LogLevel},
    processing_rule::{deserialize_processing_rules, ProcessingRule},
    service_mapping::deserialize_service_mapping,
};

/// `FallbackConfig` is a struct that represents fields that are not supported in the extension yet.
///
/// If `extension_version` is set to `compatibility`, the Go extension will be launched.
#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
#[allow(clippy::struct_excessive_bools)]
pub struct FallbackConfig {
    extension_version: Option<String>,
    serverless_appsec_enabled: bool,
    appsec_enabled: bool,
    profiling_enabled: bool,
    // otel
    trace_otel_enabled: bool,
    otlp_config_receiver_protocols_http_endpoint: Option<String>,
    otlp_config_receiver_protocols_grpc_endpoint: Option<String>,
    // intake urls
    url: Option<String>,
    dd_url: Option<String>,
    logs_config_logs_dd_url: Option<String>,
    // APM, as opposed to logs, does not use the `apm_config` prefix for env vars
    apm_dd_url: Option<String>,
}

/// `FallbackYamlConfig` is a struct that represents fields in `datadog.yaml` not yet supported in the extension yet.
///
#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct FallbackYamlConfig {
    logs_config: Option<Value>,
    apm_config: Option<Value>,
    otlp_config: Option<Value>,
}
#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct YamlLogsConfig {
    #[serde(deserialize_with = "deserialize_processing_rules")]
    processing_rules: Option<Vec<ProcessingRule>>,
}

/// `YamlConfig` is a struct that represents some of the fields in the datadog.yaml file.
///
/// It is used to deserialize the datadog.yaml file into a struct that can be merged with the Config struct.
#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct YamlConfig {
    pub logs_config: YamlLogsConfig,
    pub proxy: YamlProxyConfig,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct YamlProxyConfig {
    pub https: Option<String>,
    pub no_proxy: Option<Vec<String>>,
}

#[derive(Debug, PartialEq, Deserialize, Clone)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub site: String,
    pub api_key: String,
    pub api_key_secret_arn: String,
    pub kms_api_key: String,
    pub env: Option<String>,
    pub service: Option<String>,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub version: Option<String>,
    pub tags: Option<String>,
    #[serde(deserialize_with = "deserialize_log_level")]
    pub log_level: LogLevel,
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub logs_config_processing_rules: Option<Vec<ProcessingRule>>,
    pub serverless_flush_strategy: FlushStrategy,
    pub enhanced_metrics: bool,
    pub flush_timeout: u64, //TODO go agent adds jitter too
    pub https_proxy: Option<String>,
    pub capture_lambda_payload: bool,
    pub capture_lambda_payload_max_depth: u32,
    #[serde(deserialize_with = "deserialize_service_mapping")]
    pub service_mapping: HashMap<String, String>,
    pub serverless_logs_enabled: bool,
    // Trace Propagation
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style: Vec<TracePropagationStyle>,
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style_extract: Vec<TracePropagationStyle>,
    pub trace_propagation_extract_first: bool,
    pub trace_propagation_http_baggage_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // General
            site: String::default(),
            api_key: String::default(),
            api_key_secret_arn: String::default(),
            kms_api_key: String::default(),
            serverless_flush_strategy: FlushStrategy::Default,
            flush_timeout: 5,
            // Unified Tagging
            env: None,
            service: None,
            version: None,
            tags: None,
            // Logs
            log_level: LogLevel::default(),
            logs_config_processing_rules: None,
            // Metrics
            enhanced_metrics: true,
            https_proxy: None,
            capture_lambda_payload: false,
            capture_lambda_payload_max_depth: 10,
            service_mapping: HashMap::new(),
            serverless_logs_enabled: true,
            // Trace Propagation
            trace_propagation_style: vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
            ],
            trace_propagation_style_extract: vec![],
            trace_propagation_extract_first: false,
            trace_propagation_http_baggage_enabled: false,
        }
    }
}

#[derive(Debug, PartialEq)]
#[allow(clippy::module_name_repetitions)]
pub enum ConfigError {
    ParseError(String),
    UnsupportedField(String),
}

fn log_fallback_reason(reason: &str) {
    println!("{{\"DD_EXTENSION_FALLBACK_REASON\":\"{reason}\"}}");
}

fn fallback(figment: &Figment, yaml_figment: &Figment) -> Result<(), ConfigError> {
    let (config, yaml_config): (FallbackConfig, FallbackYamlConfig) =
        match (figment.extract(), yaml_figment.extract()) {
            (Ok(env_config), Ok(yaml_config)) => (env_config, yaml_config),
            (_, Err(err)) | (Err(err), _) => {
                println!("Failed to parse Datadog config: {err}");
                return Err(ConfigError::ParseError(err.to_string()));
            }
        };

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

    if config.serverless_appsec_enabled || config.appsec_enabled {
        log_fallback_reason("appsec_enabled");
        return Err(ConfigError::UnsupportedField("appsec_enabled".to_string()));
    }

    if config.profiling_enabled {
        log_fallback_reason("profiling_enabled");
        return Err(ConfigError::UnsupportedField(
            "profiling_enabled".to_string(),
        ));
    }

    // OTEL env
    if config.trace_otel_enabled
        || config
            .otlp_config_receiver_protocols_http_endpoint
            .is_some()
        || config
            .otlp_config_receiver_protocols_grpc_endpoint
            .is_some()
        || yaml_config.otlp_config.is_some()
    {
        log_fallback_reason("otel");
        return Err(ConfigError::UnsupportedField("otel".to_string()));
    }

    // Intake URLs
    if config.url.is_some()
        || config.dd_url.is_some()
        || config.logs_config_logs_dd_url.is_some()
        || config.apm_dd_url.is_some()
        || yaml_config
            .logs_config
            .is_some_and(|c| c.get("logs_dd_url").is_some())
        || yaml_config
            .apm_config
            .is_some_and(|c| c.get("apm_dd_url").is_some())
    {
        log_fallback_reason("intake_urls");
        return Err(ConfigError::UnsupportedField("intake_urls".to_string()));
    }

    Ok(())
}

#[allow(clippy::module_name_repetitions)]
pub fn get_config(config_directory: &Path) -> Result<Config, ConfigError> {
    let path = config_directory.join("datadog.yaml");

    // Get default config fields (and ENV specific ones)
    let figment = Figment::new()
        .merge(Yaml::file(&path))
        .merge(Env::prefixed("DATADOG_"))
        .merge(Env::prefixed("DD_"))
        .merge(Env::raw().only(&["HTTPS_PROXY"]));

    // Get YAML nested fields
    let yaml_figment = Figment::from(Yaml::file(&path));

    fallback(&figment, &yaml_figment)?;

    let (mut config, yaml_config): (Config, YamlConfig) =
        match (figment.extract(), yaml_figment.extract()) {
            (Ok(env_config), Ok(yaml_config)) => (env_config, yaml_config),
            (_, Err(err)) | (Err(err), _) => {
                println!("Failed to parse Datadog config: {err}");
                return Err(ConfigError::ParseError(err.to_string()));
            }
        };
    // Set site if empty
    if config.site.is_empty() {
        config.site = "datadoghq.com".to_string();
    }

    // NOTE: Must happen after config.site is set
    // Prefer DD_PROXY_HTTPS over HTTPS_PROXY
    // No else needed as HTTPS_PROXY is handled by reqwest and built into trace client
    if let Ok(https_proxy) = std::env::var("DD_PROXY_HTTPS").or_else(|_| {
        yaml_config
            .proxy
            .https
            .clone()
            .ok_or(std::env::VarError::NotPresent)
    }) {
        if std::env::var("NO_PROXY").map_or(false, |no_proxy| no_proxy.contains(&config.site))
            || yaml_config
                .proxy
                .no_proxy
                .map_or(false, |no_proxy| no_proxy.contains(&config.site))
        {
            config.https_proxy = None;
        } else {
            config.https_proxy = Some(https_proxy);
        }
    }

    // Merge YAML nested fields
    //
    // Set logs_config_processing_rules if not defined in env
    if config.logs_config_processing_rules.is_none() {
        if let Some(processing_rules) = yaml_config.logs_config.processing_rules {
            config.logs_config_processing_rules = Some(processing_rules);
        }
    }

    // Trace Propagation
    //
    // If not set by the user, set defaults
    if config.trace_propagation_style_extract.is_empty() {
        config
            .trace_propagation_style_extract
            .clone_from(&config.trace_propagation_style);
    }

    Ok(config)
}

fn deserialize_string_or_int<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
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

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct AwsConfig {
    pub region: String,
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub aws_session_token: String,
    pub function_name: String,
    pub sandbox_init_time: Instant,
    pub aws_container_credentials_full_uri: String,
    pub aws_container_authorization_token: String,
}

#[must_use]
pub fn get_aws_partition_by_region(region: &str) -> String {
    match region {
        r if r.starts_with("us-gov-") => "aws-us-gov".to_string(),
        r if r.starts_with("cn-") => "aws-cn".to_string(),
        _ => "aws".to_string(),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use crate::config::flush_strategy::PeriodicStrategy;
    use crate::config::processing_rule;

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
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
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
                      http:
                        endpoint: localhost:4138
            ",
            )?;

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(config, ConfigError::UnsupportedField("otel".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_fallback_on_otel_yaml_empty_section() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                otlp_config:
                  receiver:
                    protocols:
                      http:
            ",
            )?;

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(config, ConfigError::UnsupportedField("otel".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_fallback_on_intake_urls() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_APM_DD_URL", "some_url");

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("intake_urls".to_string())
            );
            Ok(())
        });
    }

    #[test]
    fn test_fallback_on_intake_urls_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                apm_config:
                  apm_dd_url: some_url
            ",
            )?;

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("intake_urls".to_string())
            );
            Ok(())
        });
    }

    #[test]
    fn test_allowed_but_disabled() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_APPSEC_ENABLED", "true");

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("appsec_enabled".to_string())
            );
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
            assert_eq!(config.https_proxy, Some("my-proxy:3128".to_string()));
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
            assert_eq!(config.https_proxy, None);
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
            assert_eq!(config.https_proxy, Some("my-proxy:3128".to_string()));
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
            assert_eq!(config.https_proxy, None);
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
    fn parse_dd_version() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_VERSION", "123");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config.version.expect("failed to parse DD_VERSION"), "123");
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
}
