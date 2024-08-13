pub mod flush_strategy;
pub mod log_level;
pub mod object_ignore;
pub mod processing_rule;

use std::path::Path;

use figment::{providers::Env, Figment};
use serde::Deserialize;

use crate::config::flush_strategy::FlushStrategy;
use crate::config::log_level::LogLevel;
use crate::config::object_ignore::ObjectIgnore;
use crate::config::processing_rule::{deserialize_processing_rules, ProcessingRule};

#[derive(Debug, PartialEq, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub site: String,
    pub api_key: String,
    pub api_key_secret_arn: String,
    pub kms_api_key: String,
    pub env: Option<String>,
    pub service: Option<String>,
    pub version: Option<String>,
    pub tags: Option<String>,
    pub log_level: LogLevel,
    pub serverless_logs_enabled: bool,
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub logs_config_processing_rules: Option<Vec<ProcessingRule>>,
    pub apm_enabled: bool,
    pub apm_replace_tags: Option<ObjectIgnore>,
    pub lambda_handler: String,
    pub serverless_flush_strategy: FlushStrategy,
    pub trace_enabled: bool,
    pub serverless_trace_enabled: bool,
    pub capture_lambda_payload: bool,
    // ALL ENV VARS below are deprecated or not used by the extension
    // just here so we don't failover
    pub flush_to_log: bool,
    pub logs_injection: bool,
    pub merge_xray_traces: bool,
    pub serverless_appsec_enabled: bool,
    pub extension_version: Option<String>,
    pub enhanced_metrics: bool,
    pub cold_start_tracing: bool,
    pub min_cold_start_duration: String,
    pub cold_start_trace_skip_lib: String,
    pub service_mapping: Option<String>,
    pub data_streams_enabled: bool,
    pub trace_disabled_plugins: Option<String>,
    pub trace_debug: bool,
    pub profiling_enabled: bool,
    pub git_commit_sha: Option<String>,
    pub logs_enabled: bool,
    pub app_key: Option<String>,
    pub trace_sample_rate: Option<f64>,
    pub dotnet_tracer_home: Option<String>,
    pub trace_managed_services: bool,
    pub runtime_metrics_enabled: bool,
    pub git_repository_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // General
            site: "datadoghq.com".to_string(),
            api_key: String::default(),
            api_key_secret_arn: String::default(),
            kms_api_key: String::default(),
            serverless_flush_strategy: FlushStrategy::Default,
            // Unified Tagging
            env: None,
            service: None,
            version: None,
            tags: None,
            // Logs
            log_level: LogLevel::default(),
            logs_enabled: false, // IGNORED, this is the main agent config, but never used in
            // severless
            serverless_logs_enabled: true,
            // TODO(duncanista): Add serializer for YAML
            logs_config_processing_rules: None,
            // APM
            apm_enabled: false,
            apm_replace_tags: None,
            trace_disabled_plugins: None,
            lambda_handler: String::default(),
            serverless_trace_enabled: true,
            trace_enabled: true,
            // Ignored by the extension for now
            capture_lambda_payload: false,
            flush_to_log: false,
            logs_injection: false,
            merge_xray_traces: false,
            serverless_appsec_enabled: false,
            cold_start_tracing: true,
            min_cold_start_duration: String::default(),
            cold_start_trace_skip_lib: String::default(),
            // Failover
            extension_version: None,
            enhanced_metrics: true,
            service_mapping: None,
            data_streams_enabled: false,
            trace_debug: false,
            profiling_enabled: false,
            git_commit_sha: None,
            app_key: None,
            trace_sample_rate: None,
            dotnet_tracer_home: None,
            trace_managed_services: true,
            runtime_metrics_enabled: false,
            git_repository_url: None,
        }
    }
}

#[derive(Debug, PartialEq)]
#[allow(clippy::module_name_repetitions)]
pub enum ConfigError {
    ParseError(String),
    UnsupportedField(String),
}

fn log_failover_reason(reason: &str) {
    println!("{{\"DD_EXTENSION_FAILOVER_REASON\":\"{reason}\"}}");
}

#[allow(clippy::module_name_repetitions)]
pub fn get_config(config_directory: &Path) -> Result<Config, ConfigError> {
    let path = config_directory.join("datadog.yaml");
    let figment = Figment::new()
        // .merge(Yaml::file(path))
        .merge(Env::prefixed("DATADOG_"))
        .merge(Env::prefixed("DD_"));

    let config: Config = figment.extract().map_err(|err| match err.kind {
        figment::error::Kind::UnknownField(field, _) => {
            log_failover_reason(&field.clone());
            ConfigError::UnsupportedField(field)
        }
        _ => ConfigError::ParseError(err.to_string()),
    })?;

    // TODO(duncanista): revert to using datadog.yaml when we have a proper serializer
    if path.exists() {
        log_failover_reason("datadog_yaml");
        return Err(ConfigError::UnsupportedField("datadog_yaml".to_string()));
    }

    if config.serverless_appsec_enabled {
        log_failover_reason("appsec_enabled");
        return Err(ConfigError::UnsupportedField("appsec_enabled".to_string()));
    }

    if config.profiling_enabled {
        log_failover_reason("profiling_enabled");
        return Err(ConfigError::UnsupportedField(
            "profiling_enabled".to_string(),
        ));
    }

    match config.extension_version.as_deref() {
        Some("next") => {}
        Some(_) | None => {
            log_failover_reason("extension_version");
            return Err(ConfigError::UnsupportedField(
                "extension_version".to_string(),
            ));
        }
    }

    Ok(config)
}

#[allow(clippy::module_name_repetitions)]
pub struct AwsConfig {
    pub region: String,
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub aws_session_token: String,
    pub function_name: String,
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use crate::config::flush_strategy::PeriodicStrategy;
    use crate::config::processing_rule;

    #[test]
    fn test_allowed_but_disabled() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_APPSEC_ENABLED", "true");

            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("appsec_enabled".to_string())
            );
            Ok(())
        });
    }

    #[test]
    fn test_reject_datadog_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                apm_enabled: true
                extension_version: next
            ",
            )?;
            let config = get_config(Path::new("")).expect_err("should reject datadog.yaml file");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("datadog_yaml".to_string())
            );
            Ok(())
        });
    }

    #[test]
    fn test_reject_unknown_fields_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_UNKNOWN_FIELD", "true");
            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("unknown_field".to_string())
            );
            Ok(())
        });
    }

    #[test]
    fn test_reject_without_opt_in() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("extension_version".to_string())
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_APM_ENABLED", "true");
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    apm_enabled: true,
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_end() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "end");
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    serverless_flush_strategy: FlushStrategy::End,
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_periodically() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "periodically,100000");
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    serverless_flush_strategy: FlushStrategy::Periodically(PeriodicStrategy {
                        interval: 100_000
                    }),
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_invalid() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "invalid_strategy");
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
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
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
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
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    logs_config_processing_rules: Some(vec![ProcessingRule {
                        kind: processing_rule::Kind::ExcludeAtMatch,
                        name: "exclude".to_string(),
                        pattern: "exclude".to_string(),
                        replace_placeholder: None
                    }]),
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
            Ok(())
        });
    }
}
