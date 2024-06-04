pub mod flush_strategy;
pub mod log_level;
pub mod processing_rule;

use std::collections::HashMap;
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
#[serde(default)]
pub struct Config {
    pub site: String,
    pub api_key: String,
    // DD_KMS_API_KEY is the only KMS var that does not follow the suffix pattern
    pub kms_api_key: Option<String>,
    pub kms_env_map: HashMap<String, (String, String)>,
    pub secret_arn_env_map: HashMap<String, (String, String)>,
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
            kms_api_key: None,
            kms_env_map: HashMap::default(),
            secret_arn_env_map: HashMap::default(),
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

    let kms_env_vars = map_stripped_env_by_suffix("_KMS_ENCRYPTED");
    let secret_arn_env_vars = map_stripped_env_by_suffix("_SECRET_ARN");

    let config = figment.extract().map_err(|err| match err.kind {
        figment::error::Kind::UnknownField(field, _) => ConfigError::UnsupportedField(field),
        _ => ConfigError::ParseError(err.to_string()),
    })?;

    Ok(Config {
        kms_env_map: kms_env_vars,
        secret_arn_env_map: secret_arn_env_vars,
        ..config
    })
}

fn map_stripped_env_by_suffix(suffix: &str) -> HashMap<String, (String, String)> {
    std::env::vars()
        .filter(|(k, _)| k.ends_with(suffix))
        .map(|(k, v)| {
            let key = if k.starts_with("DD_") {
                k.strip_prefix("DD_").unwrap().to_string()
            } else if k.starts_with("DATADOG_") {
                k.strip_prefix("DATADOG_").unwrap().to_string()
            } else {
                k
            };
            (key, (v, String::new()))
        })
        .collect::<HashMap<String, (String, String)>>()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use crate::config::flush_strategy::PeriodicStrategy;
    use crate::config::processing_rule;

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

    #[test]
    fn test_kms_env_var() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SOMEKEY1_KMS_ENCRYPTED", "secret1");
            jail.set_env("DD_SOMEKEY2_KMS_ENCRYPTED", "secret2");
            jail.set_env("DD_SOMEKEY3_KMS_ENCRYPTED", "secret3");
            jail.set_env("DD_SOMEKEY4_SECRET_ARN", "secret4");
            jail.set_env("DD_SOMEKEY5_SECRET_ARN", "secret5");
            let config = get_config(Path::new("")).expect("should parse config");

            let mut expected_kms_env_map = HashMap::new();
            expected_kms_env_map.insert(
                "SOMEKEY1_KMS_ENCRYPTED".to_string(),
                ("secret1".to_string(), String::new()),
            );
            expected_kms_env_map.insert(
                "SOMEKEY2_KMS_ENCRYPTED".to_string(),
                ("secret2".to_string(), String::new()),
            );
            expected_kms_env_map.insert(
                "SOMEKEY3_KMS_ENCRYPTED".to_string(),
                ("secret3".to_string(), String::new()),
            );

            let mut expected_secret_arn_env_map = HashMap::new();
            expected_secret_arn_env_map.insert(
                "SOMEKEY4_SECRET_ARN".to_string(),
                ("secret4".to_string(), String::new()),
            );
            expected_secret_arn_env_map.insert(
                "SOMEKEY5_SECRET_ARN".to_string(),
                ("secret5".to_string(), String::new()),
            );

            let mut expected_config = Config::default();
            expected_config.kms_env_map = expected_kms_env_map;
            expected_config.secret_arn_env_map = expected_secret_arn_env_map;

            assert_eq!(config, expected_config);
            Ok(())
        });
    }
}
