pub mod flush_strategy;
pub mod log_level;
pub mod processing_rule;

use std::path::Path;

use figment::{
    providers::{Env, Format, Yaml},
    Figment,
};
use serde::Deserialize;

use crate::config::flush_strategy::FlushStrategy;
use crate::config::log_level::LogLevel;
use crate::config::processing_rule::{deserialize_processing_rules, ProcessingRule};

#[derive(Debug, PartialEq, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
#[serde(default)]
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
    pub lambda_handler: String,
    pub serverless_flush_strategy: FlushStrategy,
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
            serverless_logs_enabled: true,
            // TODO(duncanista): Add serializer for YAML
            logs_config_processing_rules: None,
            // APM
            apm_enabled: false,
            lambda_handler: String::default(),
        }
    }
}

#[derive(Debug, PartialEq)]
#[allow(clippy::module_name_repetitions)]
pub enum ConfigError {
    ParseError(String),
    UnsupportedField(String),
}

#[allow(clippy::module_name_repetitions)]
pub fn get_config(config_directory: &Path) -> Result<Config, ConfigError> {
    let path = config_directory.join("datadog.yaml");
    let figment = Figment::new()
        .merge(Yaml::file(path))
        .merge(Env::prefixed("DATADOG_"))
        .merge(Env::prefixed("DD_"));

    let config = figment.extract().map_err(|err| match err.kind {
        figment::error::Kind::UnknownField(field, _) => ConfigError::UnsupportedField(field),
        _ => ConfigError::ParseError(err.to_string()),
    })?;

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
    fn test_reject_unknown_fields_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                unknown_field: true
            ",
            )?;
            let config = get_config(Path::new("")).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("unknown_field".to_string())
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
    fn test_precedence() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                apm_enabled: true
            ",
            )?;
            jail.set_env("DD_APM_ENABLED", "false");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    apm_enabled: false,
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
                apm_enabled: true
            ",
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    apm_enabled: true,
                    ..Config::default()
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_APM_ENABLED", "true");
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    apm_enabled: true,
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
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config, Config::default());
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
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
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    serverless_flush_strategy: FlushStrategy::End,
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
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    serverless_flush_strategy: FlushStrategy::Periodically(PeriodicStrategy {
                        interval: 100_000
                    }),
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
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
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
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
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
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    logs_config_processing_rules: Some(vec![ProcessingRule {
                        kind: processing_rule::Kind::ExcludeAtMatch,
                        name: "exclude".to_string(),
                        pattern: "exclude".to_string(),
                        replace_placeholder: None,
                    }]),
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
            // TODO(duncanista): Update to use YAML configuration field `logs_config`: `processing_rules`: - ...
            jail.create_file(
                "datadog.yaml",
                r#"
                logs_config_processing_rules:
                   - type: exclude_at_match
                     name: "exclude"
                     pattern: "exclude"
                   - type: include_at_match
                     name: "include"
                     pattern: "include"
                   - type: mask_sequences
                     name: "mask"
                     pattern: "mask"
                     replace_placeholder: "REPLACED"
            "#,
            )?;
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(
                config,
                Config {
                    logs_config_processing_rules: Some(vec![
                        ProcessingRule {
                            kind: processing_rule::Kind::ExcludeAtMatch,
                            name: "exclude".to_string(),
                            pattern: "exclude".to_string(),
                            replace_placeholder: None,
                        },
                        ProcessingRule {
                            kind: processing_rule::Kind::IncludeAtMatch,
                            name: "include".to_string(),
                            pattern: "include".to_string(),
                            replace_placeholder: None,
                        },
                        ProcessingRule {
                            kind: processing_rule::Kind::MaskSequences,
                            name: "mask".to_string(),
                            pattern: "mask".to_string(),
                            replace_placeholder: Some("REPLACED".to_string()),
                        },
                    ]),
                    ..Config::default()
                }
            );
            Ok(())
        });
    }
}
