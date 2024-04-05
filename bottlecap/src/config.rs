use std::path::Path;

use serde::Deserialize;

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
    pub apm_enabled: bool,
    pub logs_enabled: bool,
    pub handler: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            apm_enabled: false,
            site: "datadoqhq.com".to_string(),
            api_key: String::default(),
            env: None,
            service: None,
            version: None,
            tags: None,
            log_level: LogLevel::default(),
            logs_enabled: true,
            handler: String::default(),
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
        .merge(Env::prefixed("DD_"))
        .merge(Env::prefixed("DATADOG_"))
        .merge(Yaml::file(path));

    let config = figment.extract().map_err(|err| match err.kind {
        figment::error::Kind::UnknownField(field, _) => ConfigError::UnsupportedField(field),
        _ => ConfigError::ParseError(err.to_string()),
    })?;
    println!("{:?}", config);
    Ok(config)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_reject_unknown_fields_yaml() {
        figment::Jail::expect_with(|jail| {
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
    fn test_parse_config_file() {
        figment::Jail::expect_with(|jail| {
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
        figment::Jail::expect_with(|_| {
            let config = get_config(Path::new("")).expect("should parse config");
            assert_eq!(config, Config::default());
            Ok(())
        });
    }
}
