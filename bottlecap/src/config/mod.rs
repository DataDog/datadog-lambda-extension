pub mod flush_strategy;
pub mod log_level;
pub mod processing_rule;

use std::path::Path;

use figment::providers::{Format, Yaml};
use figment::{providers::Env, Figment};
use serde::Deserialize;

use crate::config::flush_strategy::FlushStrategy;
use crate::config::log_level::LogLevel;
use crate::config::processing_rule::{deserialize_processing_rules, ProcessingRule};

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
    pub version: Option<String>,
    pub tags: Option<String>,
    pub log_level: LogLevel,
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub logs_config_processing_rules: Option<Vec<ProcessingRule>>,
    pub serverless_flush_strategy: FlushStrategy,
    pub enhanced_metrics: bool,
    // Failover
    pub extension_version: Option<String>,
    pub serverless_appsec_enabled: bool,
    pub appsec_enabled: bool,
    pub profiling_enabled: bool,
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
            // Failover
            extension_version: None,
            serverless_appsec_enabled: false,
            appsec_enabled: false,
            profiling_enabled: false,
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

    // Get default config fields (and ENV specific ones)
    let figment = Figment::new()
        .merge(Yaml::file(&path))
        .merge(Env::prefixed("DATADOG_"))
        .merge(Env::prefixed("DD_"));

    // Get YAML nested fields
    let yaml_figment = Figment::new().merge(Yaml::file(&path));

    let (mut config, yaml_config): (Config, YamlConfig) =
        match (figment.extract(), yaml_figment.extract()) {
            (Ok(env_config), Ok(yaml_config)) => (env_config, yaml_config),
            (_, Err(err)) | (Err(err), _) => return Err(ConfigError::ParseError(err.to_string())),
        };

    // Set site if empty
    if config.site.is_empty() {
        config.site = "datadoghq.com".to_string();
    }

    // Merge YAML nested fields
    if let Some(processing_rules) = yaml_config.logs_config.processing_rules {
        config.logs_config_processing_rules = Some(processing_rules);
    }

    // Failover
    if config.serverless_appsec_enabled || config.appsec_enabled {
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
                extension_version: next
            ",
            )?;
            jail.set_env("DD_SITE", "datad0g.com");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    site: "datad0g.com".to_string(),
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
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
                extension_version: next
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    extension_version: Some("next".to_string()),
                    site: "datadoghq.com".to_string(),
                    ..Config::default()
                }
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
            jail.set_env("DD_SITE", "datadoghq.eu");
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    site: "datadoghq.eu".to_string(),
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
                    site: "datadoghq.com".to_string(),
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
                    site: "datadoghq.com".to_string(),
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
                    site: "datadoghq.com".to_string(),
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
                    site: "datadoghq.com".to_string(),
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
                    site: "datadoghq.com".to_string(),
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
                    site: "datadoghq.com".to_string(),
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
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
                extension_version: next
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
                config,
                Config {
                    logs_config_processing_rules: Some(vec![ProcessingRule {
                        kind: processing_rule::Kind::ExcludeAtMatch,
                        name: "exclude".to_string(),
                        pattern: "exclude".to_string(),
                        replace_placeholder: None
                    }]),
                    site: "datadoghq.com".to_string(),
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
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
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    site: "datadoghq.com".to_string(),
                    extension_version: Some("next".to_string()),
                    ..Config::default()
                }
            );
            Ok(())
        });
    }
}
