use std::sync::Arc;

use crate::config::{Config, aws::AwsConfig};

pub mod interceptor;

/// Returns true if the proxy should be started.
///
/// The proxy should be started if:
/// - The LWA proxy is set, or
/// - ASM is enabled and the `AWS_LAMBDA_EXEC_WRAPPER` environment variable is set to `/opt/datadog_wrapper`
#[must_use]
#[allow(clippy::module_name_repetitions)]
pub fn should_start_proxy(config: &Arc<Config>, aws_config: &Arc<AwsConfig>) -> bool {
    let lwa_proxy_set = aws_config.aws_lwa_proxy_lambda_runtime_api.is_some();
    let datadog_wrapper_set = aws_config
        .exec_wrapper
        .as_ref()
        .is_some_and(|s| s.eq("/opt/datadog_wrapper"));

    lwa_proxy_set || (datadog_wrapper_set && config.serverless_appsec_enabled)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn test_should_start_proxy_everything_set() {
        let config = Arc::new(Config {
            // Appsec is enabled, so we should start the proxy
            serverless_appsec_enabled: true,
            ..Default::default()
        });
        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".to_string(),
            aws_lwa_proxy_lambda_runtime_api: Some("127.0.0.1:12345".to_string()),
            function_name: String::new(),
            runtime_api: String::new(),
            sandbox_init_time: Instant::now(),
            exec_wrapper: Some("/opt/datadog_wrapper".to_string()),
        });
        assert!(should_start_proxy(&config, &aws_config));
    }
    #[test]
    fn test_should_start_proxy_lwa_proxy_set() {
        let config = Arc::new(Config::default());
        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".to_string(),
            // LWA proxy is set, so we should start the proxy
            aws_lwa_proxy_lambda_runtime_api: Some("127.0.0.1:12345".to_string()),
            function_name: String::new(),
            runtime_api: String::new(),
            sandbox_init_time: Instant::now(),
            exec_wrapper: None,
        });
        assert!(should_start_proxy(&config, &aws_config));
    }

    #[test]
    fn test_should_start_proxy_appsec_enabled_and_datadog_wrapper_set() {
        let config = Arc::new(Config {
            // Appsec is enabled, so we should start the proxy
            serverless_appsec_enabled: true,
            ..Default::default()
        });
        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".to_string(),
            aws_lwa_proxy_lambda_runtime_api: None,
            function_name: String::new(),
            runtime_api: String::new(),
            sandbox_init_time: Instant::now(),
            exec_wrapper: Some("/opt/datadog_wrapper".to_string()),
        });
        assert!(should_start_proxy(&config, &aws_config));
    }

    #[test]
    fn test_should_start_proxy_appsec_disabled_and_datadog_wrapper_set() {
        let config = Arc::new(Config {
            // Appsec is disabled, so we should not start the proxy
            serverless_appsec_enabled: false,
            ..Default::default()
        });
        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".to_string(),
            aws_lwa_proxy_lambda_runtime_api: None,
            function_name: String::new(),
            runtime_api: String::new(),
            sandbox_init_time: Instant::now(),
            exec_wrapper: Some("/opt/datadog_wrapper".to_string()),
        });
        assert!(!should_start_proxy(&config, &aws_config));
    }

    #[test]
    fn test_should_start_proxy_appsec_enabled_datadog_wrapper_not_set() {
        let config = Arc::new(Config {
            // Appsec is enabled, so we should not start the proxy
            serverless_appsec_enabled: true,
            ..Default::default()
        });
        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".to_string(),
            aws_lwa_proxy_lambda_runtime_api: None,
            function_name: String::new(),
            runtime_api: String::new(),
            sandbox_init_time: Instant::now(),
            // Datadog wrapper is not set, so we should not start the proxy
            exec_wrapper: Some("/opt/not_datadog".to_string()),
        });
        assert!(!should_start_proxy(&config, &aws_config));
    }
}
