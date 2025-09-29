use std::sync::Arc;

use crate::config::Config;

pub mod agent;
pub mod processor;
pub mod transform;

#[must_use]
pub fn should_enable_otlp_agent(config: &Arc<Config>) -> bool {
    config.otlp_config_traces_enabled
        && config
            .otlp_config_receiver_protocols_http_endpoint
            .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use crate::config::get_config;

    #[test]
    fn test_should_enable_otlp_agent_from_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                otlp_config:
                  receiver:
                    protocols:
                      http:
                        endpoint: 0.0.0.0:4318
            ",
            )?;

            let config = get_config(Path::new("")).expect("should parse config");

            // Since the default for traces is `true`, we don't need to set it.
            assert!(should_enable_otlp_agent(&Arc::new(config)));

            Ok(())
        });
    }

    #[test]
    fn test_should_enable_otlp_agent_from_env_vars() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "0.0.0.0:4318",
            );

            let config = get_config(Path::new("")).expect("should parse config");

            // Since the default for traces is `true`, we don't need to set it.
            assert!(should_enable_otlp_agent(&Arc::new(config)));

            Ok(())
        });
    }

    #[test]
    fn test_should_not_enable_otlp_agent_if_traces_are_disabled() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_OTLP_CONFIG_TRACES_ENABLED", "false");
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "0.0.0.0:4318",
            );

            let config = get_config(Path::new("")).expect("should parse config");

            assert!(!should_enable_otlp_agent(&Arc::new(config)));

            Ok(())
        });
    }
}
