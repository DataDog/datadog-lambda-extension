use std::sync::Arc;

use crate::config::Config;

pub mod agent;
pub mod processor;
pub mod transform;

#[must_use]
pub fn should_enable_otlp_agent(config: &Arc<Config>) -> bool {
    should_enable_otlp_traces(config) || should_enable_otlp_metrics(config)
}

#[must_use]
pub fn should_enable_otlp_traces(config: &Config) -> bool {
    config.otlp_config_traces_enabled
        && (config
            .otlp_config_receiver_protocols_http_endpoint
            .is_some()
            || config
                .otlp_config_receiver_protocols_grpc_endpoint
                .is_some())
}

/// Metrics are forwarded to Datadog's OTLP intake as-is, over HTTP only. They don't need an
/// endpoint to be configured: the listener falls back to the tracers' default port.
#[must_use]
pub fn should_enable_otlp_metrics(config: &Config) -> bool {
    config.otlp_config_metrics_enabled || config.ext.metrics_otel_enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    use crate::config::get_config;

    #[test]
    fn test_should_enable_otlp_agent_with_http_from_yaml() {
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

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_agent(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_enable_otlp_agent_with_http_from_env_vars() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "0.0.0.0:4318",
            );

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_agent(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_enable_otlp_agent_with_grpc_from_env_vars() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_ENDPOINT",
                "0.0.0.0:4317",
            );

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_agent(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_enable_otlp_agent_with_both_protocols() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "0.0.0.0:4318",
            );
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_ENDPOINT",
                "0.0.0.0:4317",
            );

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_agent(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_not_enable_otlp_agent_if_traces_disabled() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_OTLP_CONFIG_TRACES_ENABLED", "false");
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "0.0.0.0:4318",
            );
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_ENDPOINT",
                "0.0.0.0:4317",
            );

            let config = Arc::new(get_config(Path::new("")));
            assert!(!should_enable_otlp_agent(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_not_enable_otlp_agent_without_endpoints() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            let config = Arc::new(get_config(Path::new("")));
            assert!(!should_enable_otlp_agent(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_enable_otlp_metrics_from_otlp_config_without_endpoints() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_OTLP_CONFIG_METRICS_ENABLED", "true");

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_agent(&config));
            assert!(should_enable_otlp_metrics(&config));
            assert!(!should_enable_otlp_traces(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_enable_otlp_metrics_from_tracer_flag() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_METRICS_OTEL_ENABLED", "true");

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_agent(&config));
            assert!(should_enable_otlp_metrics(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_enable_otlp_metrics_with_traces_disabled() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_OTLP_CONFIG_TRACES_ENABLED", "false");
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "0.0.0.0:4318",
            );
            jail.set_env("DD_METRICS_OTEL_ENABLED", "true");

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_agent(&config));
            assert!(!should_enable_otlp_traces(&config));
            assert!(should_enable_otlp_metrics(&config));

            Ok(())
        });
    }

    #[test]
    fn test_should_not_enable_otlp_metrics_by_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "0.0.0.0:4318",
            );

            let config = Arc::new(get_config(Path::new("")));
            assert!(should_enable_otlp_traces(&config));
            assert!(!should_enable_otlp_metrics(&config));

            Ok(())
        });
    }
}
