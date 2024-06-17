// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::Endpoint;
use std::borrow::Cow;
use std::env;
use std::str::FromStr;

use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_utils::config_utils::{
    read_cloud_env, trace_intake_url, trace_intake_url_prefixed, trace_stats_url,
    trace_stats_url_prefixed,
};
use datadog_trace_utils::trace_utils;

#[derive(Debug)]
pub struct Config {
    pub dd_site: String,
    pub env_type: trace_utils::EnvironmentType,
    pub function_name: Option<String>,
    pub max_request_content_length: usize,
    pub mini_agent_version: String,
    pub obfuscation_config: obfuscation_config::ObfuscationConfig,
    pub os: String,
    /// how often to flush stats, in seconds
    pub stats_flush_interval: u64,
    /// how often to flush traces, in seconds
    pub trace_flush_interval: u64,
    pub trace_intake: Endpoint,
    pub trace_stats_intake: Endpoint,
    /// timeout for environment verification, in milliseconds
    pub verify_env_timeout: u64,
}

impl Config {
    pub fn new() -> Result<Config, Box<dyn std::error::Error>> {
        let api_key: Cow<str> = env::var("DD_API_KEY")
            .map_err(|_| anyhow::anyhow!("DD_API_KEY environment variable is not set"))?
            .into();

        let (function_name, env_type) = read_cloud_env().ok_or_else(|| {
            anyhow::anyhow!("Unable to identify environment. Shutting down Mini Agent.")
        })?;

        let dd_site = env::var("DD_SITE").unwrap_or_else(|_| "datadoghq.com".to_string());

        // construct the trace & trace stats intake urls based on DD_SITE env var (to flush traces &
        // trace stats to)
        let mut trace_intake_url = trace_intake_url(&dd_site);
        let mut trace_stats_intake_url = trace_stats_url(&dd_site);

        // DD_APM_DD_URL env var will primarily be used for integration tests
        // overrides the entire trace/trace stats intake url prefix
        if let Ok(endpoint_prefix) = env::var("DD_APM_DD_URL") {
            trace_intake_url = trace_intake_url_prefixed(&endpoint_prefix);
            trace_stats_intake_url = trace_stats_url_prefixed(&endpoint_prefix);
        };

        let obfuscation_config = obfuscation_config::ObfuscationConfig::new().map_err(|err| {
            anyhow::anyhow!(
                "Error creating obfuscation config, Mini Agent will not start. Error: {err}",
            )
        })?;

        let mini_agent_version: String = env!("CARGO_PKG_VERSION").to_string();

        Ok(Config {
            function_name: Some(function_name),
            env_type,
            os: env::consts::OS.to_string(),
            max_request_content_length: 10 * 1024 * 1024, // 10MB in Bytes
            trace_flush_interval: 3,
            stats_flush_interval: 3,
            verify_env_timeout: 100,
            dd_site,
            trace_intake: Endpoint {
                url: hyper::Uri::from_str(&trace_intake_url).unwrap(),
                api_key: Some(api_key.clone()),
            },
            trace_stats_intake: Endpoint {
                url: hyper::Uri::from_str(&trace_stats_intake_url).unwrap(),
                api_key: Some(api_key),
            },
            obfuscation_config,
            mini_agent_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;
    use serial_test::serial;
    use std::env;

    use crate::config;

    #[test]
    #[serial]
    fn test_error_if_unable_to_identify_env() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");

        let config = config::Config::new();
        assert!(config.is_err());
        assert_eq!(
            config.unwrap_err().to_string(),
            "Unable to identify environment. Shutting down Mini Agent."
        );
        env::remove_var("DD_API_KEY");
    }

    #[test]
    #[serial]
    fn test_error_if_no_api_key_env_var() {
        let config = config::Config::new();
        assert!(config.is_err());
        assert_eq!(
            config.unwrap_err().to_string(),
            "DD_API_KEY environment variable is not set"
        );
    }

    #[test]
    #[serial]
    fn test_default_trace_and_trace_stats_urls() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");
        env::set_var("K_SERVICE", "function_name");
        let config_res = config::Config::new();
        assert!(config_res.is_ok());
        let config = config_res.unwrap();
        assert_eq!(
            config.trace_intake.url,
            "https://trace.agent.datadoghq.com/api/v0.2/traces"
        );
        assert_eq!(
            config.trace_stats_intake.url,
            "https://trace.agent.datadoghq.com/api/v0.2/stats"
        );
        env::remove_var("DD_API_KEY");
        env::remove_var("K_SERVICE");
    }

    #[duplicate_item(
        test_name                       dd_site                 expected_url;
        [test_us1_trace_intake_url]     ["datadoghq.com"]       ["https://trace.agent.datadoghq.com/api/v0.2/traces"];
        [test_us3_trace_intake_url]     ["us3.datadoghq.com"]   ["https://trace.agent.us3.datadoghq.com/api/v0.2/traces"];
        [test_us5_trace_intake_url]     ["us5.datadoghq.com"]   ["https://trace.agent.us5.datadoghq.com/api/v0.2/traces"];
        [test_eu_trace_intake_url]      ["datadoghq.eu"]        ["https://trace.agent.datadoghq.eu/api/v0.2/traces"];
        [test_ap1_trace_intake_url]     ["ap1.datadoghq.com"]   ["https://trace.agent.ap1.datadoghq.com/api/v0.2/traces"];
        [test_gov_trace_intake_url]     ["ddog-gov.com"]        ["https://trace.agent.ddog-gov.com/api/v0.2/traces"];
    )]
    #[test]
    #[serial]
    fn test_name() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");
        env::set_var("K_SERVICE", "function_name");
        env::set_var("DD_SITE", dd_site);
        let config_res = config::Config::new();
        assert!(config_res.is_ok());
        let config = config_res.unwrap();
        assert_eq!(config.trace_intake.url, expected_url);
        env::remove_var("DD_API_KEY");
        env::remove_var("DD_SITE");
        env::remove_var("K_SERVICE");
    }

    #[duplicate_item(
        test_name                       dd_site                 expected_url;
        [test_us1_trace_stats_intake_url]     ["datadoghq.com"]       ["https://trace.agent.datadoghq.com/api/v0.2/stats"];
        [test_us3_trace_stats_intake_url]     ["us3.datadoghq.com"]   ["https://trace.agent.us3.datadoghq.com/api/v0.2/stats"];
        [test_us5_trace_stats_intake_url]     ["us5.datadoghq.com"]   ["https://trace.agent.us5.datadoghq.com/api/v0.2/stats"];
        [test_eu_trace_stats_intake_url]      ["datadoghq.eu"]        ["https://trace.agent.datadoghq.eu/api/v0.2/stats"];
        [test_ap1_trace_stats_intake_url]     ["ap1.datadoghq.com"]   ["https://trace.agent.ap1.datadoghq.com/api/v0.2/stats"];
        [test_gov_trace_stats_intake_url]     ["ddog-gov.com"]        ["https://trace.agent.ddog-gov.com/api/v0.2/stats"];
    )]
    #[test]
    #[serial]
    fn test_name() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");
        env::set_var("K_SERVICE", "function_name");
        env::set_var("DD_SITE", dd_site);
        let config_res = config::Config::new();
        assert!(config_res.is_ok());
        let config = config_res.unwrap();
        assert_eq!(config.trace_stats_intake.url, expected_url);
        env::remove_var("DD_API_KEY");
        env::remove_var("DD_SITE");
        env::remove_var("K_SERVICE");
    }

    #[test]
    #[serial]
    fn test_set_custom_trace_and_trace_stats_intake_url() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");
        env::set_var("K_SERVICE", "function_name");
        env::set_var("DD_APM_DD_URL", "http://127.0.0.1:3333");
        let config_res = config::Config::new();
        assert!(config_res.is_ok());
        let config = config_res.unwrap();
        assert_eq!(
            config.trace_intake.url,
            "http://127.0.0.1:3333/api/v0.2/traces"
        );
        assert_eq!(
            config.trace_stats_intake.url,
            "http://127.0.0.1:3333/api/v0.2/stats"
        );
        env::remove_var("DD_API_KEY");
        env::remove_var("DD_APM_DD_URL");
        env::remove_var("K_SERVICE");
    }
}
