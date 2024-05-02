use std::path::Path;

use serde::{Deserialize, Deserializer, self};
use tracing::debug;

use figment::{
    providers::{Env, Format, Yaml},
    Figment,
};

#[derive(Debug, PartialEq, Deserialize, Default)]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

#[derive(Debug, PartialEq)]
pub struct PeriodicStrategy {
    pub interval: u64,
}

#[derive(Debug, PartialEq)]
pub enum FlushStrategy {
    Default,
    End,
    Periodically(PeriodicStrategy),
}

// Deserialize for FlushStrategy
// Flush Strategy can be either "end" or "periodically,<ms>"
impl<'de> Deserialize<'de> for FlushStrategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "end" => Ok(FlushStrategy::End),
            _ => {
                let mut split_value = value.as_str().split(',');
                // "periodically,60000"
                match split_value.next() {
                    Some(first_value) if first_value.starts_with("periodically") => {
                        let interval = split_value.next();
                        // "60000"
                        match interval {
                            Some(interval) => {
                                let interval = interval.parse().map_err(serde::de::Error::custom)?;
                                Ok(FlushStrategy::Periodically(PeriodicStrategy { interval }))
                            },
                            None => {
                                debug!("invalid flush strategy: {}, using default", value);
                                Ok(FlushStrategy::Default)
                            }
                        }
                    },
                    _ => {
                        debug!("invalid flush strategy: {}, using default", value);
                        Ok(FlushStrategy::Default)
                    }
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct Config {
    pub site: String,
    pub api_key: String,
    pub env: Option<String>,
    pub service: Option<String>,
    pub version: Option<String>,
    pub tags: Option<String>,
    pub log_level: LogLevel,
    pub serverless_logs_enabled: bool,
    pub apm_enabled: bool,
    pub lambda_handler: String,
    pub serverless_flush_strategy: FlushStrategy,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // General
            site: "datadoqhq.com".to_string(),
            api_key: String::default(),
            serverless_flush_strategy: FlushStrategy::Default,
            // Unified Tagging
            env: None,
            service: None,
            version: None,
            tags: None,
            // Logs
            log_level: LogLevel::default(),
            serverless_logs_enabled: true,
            // APM
            apm_enabled: false,
            lambda_handler: String::default(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum ConfigError {
    ParseError(String),
    UnsupportedField(String),
}

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

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_reject_unknown_fields_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r#"
                unknown_field: true
            "#,
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
                r#"
                apm_enabled: true
            "#,
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
                r#"
                apm_enabled: true
            "#,
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
}
