pub mod apm_replace_rule;
pub mod env;
pub mod flush_strategy;
pub mod log_level;
pub mod processing_rule;
pub mod service_mapping;
pub mod trace_propagation_style;
pub mod yaml;

use datadog_trace_utils::config_utils::{trace_intake_url, trace_intake_url_prefixed};
use std::path::Path;
use std::time::Instant;

use figment::providers::{Format, Yaml};
use figment::{providers::Env, Figment};

use crate::config::{
    apm_replace_rule::deserialize_apm_replace_rules,
    env::Config as EnvConfig,
    processing_rule::{deserialize_processing_rules, ProcessingRule},
    yaml::Config as YamlConfig,
};

pub type Config = EnvConfig;

#[derive(Debug, PartialEq)]
#[allow(clippy::module_name_repetitions)]
pub enum ConfigError {
    ParseError(String),
    UnsupportedField(String),
}

fn log_fallback_reason(reason: &str) {
    println!("{{\"DD_EXTENSION_FALLBACK_REASON\":\"{reason}\"}}");
}

fn fallback(config: &EnvConfig, yaml_config: &YamlConfig, region: &str) -> Result<(), ConfigError> {
    // Customer explicitly opted out of the Next Gen extension
    let opted_out = match config.extension_version.as_deref() {
        Some("compatibility") => true,
        // We want customers using the `next` to not be affected
        _ => false,
    };

    if opted_out {
        log_fallback_reason("extension_version");
        return Err(ConfigError::UnsupportedField(
            "extension_version".to_string(),
        ));
    }

    if config.serverless_appsec_enabled || config.appsec_enabled {
        log_fallback_reason("appsec_enabled");
        return Err(ConfigError::UnsupportedField("appsec_enabled".to_string()));
    }

    // OTLP
    let has_otlp_env_config = config
        .otlp_config_receiver_protocols_grpc_endpoint
        .is_some()
        || config
            .otlp_config_receiver_protocols_grpc_transport
            .is_some()
        || config
            .otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
            .is_some()
        || config.otlp_config_metrics_enabled
        || config.otlp_config_metrics_resource_attributes_as_tags
        || config.otlp_config_metrics_instrumentation_scope_metadata_as_tags
        || config.otlp_config_metrics_tag_cardinality.is_some()
        || config.otlp_config_metrics_delta_ttl.is_some()
        || config.otlp_config_metrics_histograms_mode.is_some()
        || config.otlp_config_metrics_histograms_send_count_sum_metrics
        || config.otlp_config_metrics_histograms_send_aggregation_metrics
        || config
            .otlp_config_metrics_sums_cumulative_monotonic_mode
            .is_some()
        || config
            .otlp_config_metrics_sums_initial_cumulativ_monotonic_value
            .is_some()
        || config.otlp_config_metrics_summaries_mode.is_some()
        || config
            .otlp_config_traces_probabilistic_sampler_sampling_percentage
            .is_some()
        || config.otlp_config_logs_enabled;

    let has_otlp_yaml_config = yaml_config.otlp_config.receiver.protocols.grpc.is_some()
        || yaml_config
            .otlp_config
            .traces
            .probabilistic_sampler
            .is_some()
        || yaml_config.otlp_config.logs.is_some();

    if has_otlp_env_config || has_otlp_yaml_config {
        log_fallback_reason("otel");
        return Err(ConfigError::UnsupportedField("otel".to_string()));
    }

    // Govcloud Regions
    if region.starts_with("us-gov-") {
        log_fallback_reason("gov_region");
        return Err(ConfigError::UnsupportedField("gov_region".to_string()));
    }

    Ok(())
}

#[allow(clippy::module_name_repetitions)]
pub fn get_config(config_directory: &Path, region: &str) -> Result<EnvConfig, ConfigError> {
    let path = config_directory.join("datadog.yaml");

    // Get default config fields (and ENV specific ones)
    let figment = Figment::new()
        .merge(Yaml::file(&path))
        .merge(Env::prefixed("DATADOG_"))
        .merge(Env::prefixed("DD_"))
        .merge(Env::raw().only(&["HTTPS_PROXY"]));

    // Get YAML nested fields
    let yaml_figment = Figment::from(Yaml::file(&path));

    let (mut config, yaml_config): (EnvConfig, YamlConfig) =
        match (figment.extract(), yaml_figment.extract()) {
            (Ok(env_config), Ok(yaml_config)) => (env_config, yaml_config),
            (_, Err(err)) | (Err(err), _) => {
                println!("Failed to parse Datadog config: {err}");
                return Err(ConfigError::ParseError(err.to_string()));
            }
        };

    fallback(&config, &yaml_config, region)?;

    // Set site if empty
    if config.site.is_empty() {
        config.site = "datadoghq.com".to_string();
    }

    // NOTE: Must happen after config.site is set
    // Prefer DD_PROXY_HTTPS over HTTPS_PROXY
    // No else needed as HTTPS_PROXY is handled by reqwest and built into trace client
    if let Ok(https_proxy) = std::env::var("DD_PROXY_HTTPS").or_else(|_| {
        yaml_config
            .proxy
            .https
            .clone()
            .ok_or(std::env::VarError::NotPresent)
    }) {
        let no_proxy = yaml_config.proxy.no_proxy.clone();
        if std::env::var("NO_PROXY").map_or(false, |no_proxy| no_proxy.contains(&config.site))
            || no_proxy.map_or(false, |no_proxy| no_proxy.contains(&config.site))
        {
            config.https_proxy = None;
        } else {
            config.https_proxy = Some(https_proxy);
        }
    }

    merge_config(&mut config, &yaml_config);

    // Metrics are handled by dogstatsd in Main
    Ok(config)
}

/// Merge YAML nested fields into `EnvConfig`
///
fn merge_config(config: &mut EnvConfig, yaml_config: &YamlConfig) {
    // Set logs_config_processing_rules if not defined in env
    if config.logs_config_processing_rules.is_none() {
        if let Some(processing_rules) = yaml_config.logs_config.processing_rules.as_ref() {
            config.logs_config_processing_rules = Some(processing_rules.clone());
        }
    }

    // Trace Propagation
    //
    // If not set by the user, set defaults
    if config.trace_propagation_style_extract.is_empty() {
        config
            .trace_propagation_style_extract
            .clone_from(&config.trace_propagation_style);
    }
    if config.logs_config_logs_dd_url.is_empty() {
        config.logs_config_logs_dd_url = build_fqdn_logs(config.site.clone());
    }

    if config.apm_config_apm_dd_url.is_empty() {
        config.apm_config_apm_dd_url = trace_intake_url(config.site.clone().as_str());
    } else {
        config.apm_config_apm_dd_url =
            trace_intake_url_prefixed(config.apm_config_apm_dd_url.as_str());
    }

    if config.apm_replace_tags.is_none() {
        if let Some(rules) = yaml_config.apm_config.replace_tags.as_ref() {
            config.apm_replace_tags = Some(rules.clone());
        }
    }

    if !config.apm_config_obfuscation_http_remove_paths_with_digits {
        if let Some(obfuscation) = yaml_config.apm_config.obfuscation {
            config.apm_config_obfuscation_http_remove_paths_with_digits =
                obfuscation.http.remove_paths_with_digits;
        }
    }

    if !config.apm_config_obfuscation_http_remove_query_string {
        if let Some(obfuscation) = yaml_config.apm_config.obfuscation {
            config.apm_config_obfuscation_http_remove_query_string =
                obfuscation.http.remove_query_string;
        }
    }

    // OTLP
    //
    // - Receiver / HTTP
    if config
        .otlp_config_receiver_protocols_http_endpoint
        .is_none()
        && !yaml_config
            .otlp_config
            .receiver
            .protocols
            .http
            .endpoint
            .is_empty()
    {
        config.otlp_config_receiver_protocols_http_endpoint = Some(
            yaml_config
                .otlp_config
                .receiver
                .protocols
                .http
                .endpoint
                .clone(),
        );
    }

    if !config.otlp_config_traces_enabled && yaml_config.otlp_config.traces.enabled {
        config.otlp_config_traces_enabled = true;
    }

    if !config.otlp_config_ignore_missing_datadog_fields
        && yaml_config.otlp_config.traces.ignore_missing_datadog_fields
    {
        config.otlp_config_ignore_missing_datadog_fields = true;
    }

    if !config.otlp_config_traces_span_name_as_resource_name
        && yaml_config.otlp_config.traces.span_name_as_resource_name
    {
        config.otlp_config_traces_span_name_as_resource_name = true;
    }

    if config.otlp_config_traces_span_name_remappings.is_empty()
        && !yaml_config
            .otlp_config
            .traces
            .span_name_remappings
            .is_empty()
    {
        config
            .otlp_config_traces_span_name_remappings
            .clone_from(&yaml_config.otlp_config.traces.span_name_remappings);
    }
}

#[inline]
#[must_use]
fn build_fqdn_logs(site: String) -> String {
    format!("https://http-intake.logs.{site}")
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct AwsConfig {
    pub region: String,
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub aws_session_token: String,
    pub function_name: String,
    pub sandbox_init_time: Instant,
    pub aws_container_credentials_full_uri: String,
    pub aws_container_authorization_token: String,
}

#[must_use]
pub fn get_aws_partition_by_region(region: &str) -> String {
    match region {
        r if r.starts_with("us-gov-") => "aws-us-gov".to_string(),
        r if r.starts_with("cn-") => "aws-cn".to_string(),
        _ => "aws".to_string(),
    }
}

#[cfg(test)]
pub mod tests {
    use datadog_trace_obfuscation::replacer::parse_rules_from_string;

    use super::*;

    use crate::config::flush_strategy::{FlushStrategy, PeriodicStrategy};
    use crate::config::log_level::LogLevel;
    use crate::config::processing_rule;
    use crate::config::trace_propagation_style::TracePropagationStyle;

    const MOCK_REGION: &str = "us-east-1";

    #[test]
    fn test_reject_on_opted_out() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_EXTENSION_VERSION", "compatibility");
            let config =
                get_config(Path::new(""), MOCK_REGION).expect_err("should reject unknown fields");
            assert_eq!(
                config,
                ConfigError::UnsupportedField("extension_version".to_string())
            );
            Ok(())
        });
    }
    #[test]
    fn test_reject_on_gov_region() {
        let mock_gov_region = "us-gov-east-1";
        let config =
            get_config(Path::new(""), mock_gov_region).expect_err("should reject unknown fields");
        assert_eq!(
            config,
            ConfigError::UnsupportedField("gov_region".to_string())
        );
    }

    #[test]
    fn test_fallback_on_otel() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_ENDPOINT",
                "localhost:4138",
            );

            let config =
                get_config(Path::new(""), MOCK_REGION).expect_err("should reject unknown fields");
            assert_eq!(config, ConfigError::UnsupportedField("otel".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_fallback_on_otel_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                otlp_config:
                  receiver:
                    protocols:
                      grpc:
                        endpoint: localhost:4138
            ",
            )?;

            let config =
                get_config(Path::new(""), MOCK_REGION).expect_err("should reject unknown fields");
            assert_eq!(config, ConfigError::UnsupportedField("otel".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_default_logs_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(
                config.logs_config_logs_dd_url,
                "https://http-intake.logs.datadoghq.com".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_support_pci_logs_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_LOGS_CONFIG_LOGS_DD_URL",
                "agent-http-intake-pci.logs.datadoghq.com:443",
            );

            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(
                config.logs_config_logs_dd_url,
                "agent-http-intake-pci.logs.datadoghq.com:443".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_support_pci_traces_intake_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_APM_CONFIG_APM_DD_URL",
                "https://trace-pci.agent.datadoghq.com",
            );

            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(
                config.apm_config_apm_dd_url,
                "https://trace-pci.agent.datadoghq.com/api/v0.2/traces".to_string()
            );
            Ok(())
        });
    }

    #[test]
    fn test_support_dd_dd_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_DD_URL", "custom_proxy:3128");

            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.dd_url, "custom_proxy:3128".to_string());
            Ok(())
        });
    }

    #[test]
    fn test_support_dd_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_URL", "custom_proxy:3128");

            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.url, "custom_proxy:3128".to_string());
            Ok(())
        });
    }

    #[test]
    fn test_dd_dd_url_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.dd_url, String::new());
            Ok(())
        });
    }

    #[test]
    fn test_allowed_but_disabled() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_APPSEC_ENABLED", "true");

            let config =
                get_config(Path::new(""), MOCK_REGION).expect_err("should reject unknown fields");
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
            ",
            )?;
            jail.set_env("DD_SITE", "datad0g.com");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.site, "datad0g.com");
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
            ",
            )?;
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.site, "datadoghq.com");
            Ok(())
        });
    }

    #[test]
    fn test_parse_env() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SITE", "datadoghq.eu");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.site, "datadoghq.eu");
            Ok(())
        });
    }

    #[test]
    fn test_parse_log_level() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_LOG_LEVEL", "TRACE");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.log_level, LogLevel::Trace);
            Ok(())
        });
    }

    #[test]
    fn test_parse_default() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(
                config,
                EnvConfig {
                    site: "datadoghq.com".to_string(),
                    trace_propagation_style_extract: vec![
                        TracePropagationStyle::Datadog,
                        TracePropagationStyle::TraceContext
                    ],
                    logs_config_logs_dd_url: "https://http-intake.logs.datadoghq.com".to_string(),
                    apm_config_apm_dd_url: trace_intake_url("datadoghq.com").to_string(),
                    dd_url: String::new(), // We add the prefix in main.rs
                    ..EnvConfig::default()
                }
            );
            Ok(())
        });
    }

    #[test]
    fn test_proxy_config() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_PROXY_HTTPS", "my-proxy:3128");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.https_proxy, Some("my-proxy:3128".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_noproxy_config() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SITE", "datadoghq.eu");
            jail.set_env("DD_PROXY_HTTPS", "my-proxy:3128");
            jail.set_env(
                "NO_PROXY",
                "127.0.0.1,localhost,172.16.0.0/12,us-east-1.amazonaws.com,datadoghq.eu",
            );
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse noproxy");
            assert_eq!(config.https_proxy, None);
            Ok(())
        });
    }

    #[test]
    fn test_proxy_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                proxy:
                  https: my-proxy:3128
            ",
            )?;

            let config =
                get_config(Path::new(""), MOCK_REGION).expect("should parse weird proxy config");
            assert_eq!(config.https_proxy, Some("my-proxy:3128".to_string()));
            Ok(())
        });
    }

    #[test]
    fn test_no_proxy_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                proxy:
                  https: my-proxy:3128
                  no_proxy:
                    - datadoghq.com
            ",
            )?;

            let config =
                get_config(Path::new(""), MOCK_REGION).expect("should parse weird proxy config");
            assert_eq!(config.https_proxy, None);
            // Assertion to ensure config.site runs before proxy
            // because we chenck that noproxy contains the site
            assert_eq!(config.site, "datadoghq.com");
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_end() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "end");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.serverless_flush_strategy, FlushStrategy::End);
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_periodically() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "periodically,100000");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(
                config.serverless_flush_strategy,
                FlushStrategy::Periodically(PeriodicStrategy { interval: 100_000 })
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_flush_strategy_invalid() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "invalid_strategy");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.serverless_flush_strategy, FlushStrategy::Default);
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
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.serverless_flush_strategy, FlushStrategy::Default);
            Ok(())
        });
    }

    #[test]
    fn parse_number_or_string_env_vars() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_VERSION", "123");
            jail.set_env("DD_ENV", "123456890");
            jail.set_env("DD_SERVICE", "123456");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(config.version.expect("failed to parse DD_VERSION"), "123");
            assert_eq!(config.env.expect("failed to parse DD_ENV"), "123456890");
            assert_eq!(
                config.service.expect("failed to parse DD_SERVICE"),
                "123456"
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
            jail.create_file(
                "datadog.yaml",
                r"
                extension_version: next
                logs_config:
                  processing_rules:
                    - type: exclude_at_match
                      name: exclude-me-yaml
                      pattern: exclude-me-yaml
            ",
            )?;
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(
                config.logs_config_processing_rules,
                Some(vec![ProcessingRule {
                    kind: processing_rule::Kind::ExcludeAtMatch,
                    name: "exclude".to_string(),
                    pattern: "exclude".to_string(),
                    replace_placeholder: None
                }])
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
                site: datadoghq.com
                logs_config:
                  processing_rules:
                    - type: exclude_at_match
                      name: exclude
                      pattern: exclude
            ",
            )?;
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert_eq!(
                config.logs_config_processing_rules,
                Some(vec![ProcessingRule {
                    kind: processing_rule::Kind::ExcludeAtMatch,
                    name: "exclude".to_string(),
                    pattern: "exclude".to_string(),
                    replace_placeholder: None
                }]),
            );
            Ok(())
        });
    }

    #[test]
    fn test_parse_apm_replace_tags_from_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                site: datadoghq.com
                apm_config:
                  replace_tags:
                    - name: '*'
                      pattern: 'foo'
                      repl: 'REDACTED'
            ",
            )?;
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            let rule = parse_rules_from_string(
                r#"[
                        {"name": "*", "pattern": "foo", "repl": "REDACTED"}
                    ]"#,
            )
            .expect("can't parse rules");
            assert_eq!(config.apm_replace_tags, Some(rule),);
            Ok(())
        });
    }

    #[test]
    fn test_parse_apm_http_obfuscation_from_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r"
                site: datadoghq.com
                apm_config:
                  obfuscation:
                    http:
                      remove_query_string: true
                      remove_paths_with_digits: true
            ",
            )?;
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");
            assert!(config.apm_config_obfuscation_http_remove_query_string,);
            assert!(config.apm_config_obfuscation_http_remove_paths_with_digits,);
            Ok(())
        });
    }
    #[test]
    fn test_parse_trace_propagation_style() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env(
                "DD_TRACE_PROPAGATION_STYLE",
                "datadog,tracecontext,b3,b3multi",
            );
            jail.set_env("DD_EXTENSION_VERSION", "next");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");

            let expected_styles = vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
                TracePropagationStyle::B3,
                TracePropagationStyle::B3Multi,
            ];
            assert_eq!(config.trace_propagation_style, expected_styles);
            assert_eq!(config.trace_propagation_style_extract, expected_styles);
            Ok(())
        });
    }

    #[test]
    fn test_parse_trace_propagation_style_extract() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("DD_TRACE_PROPAGATION_STYLE_EXTRACT", "datadog");
            let config = get_config(Path::new(""), MOCK_REGION).expect("should parse config");

            assert_eq!(
                config.trace_propagation_style,
                vec![
                    TracePropagationStyle::Datadog,
                    TracePropagationStyle::TraceContext,
                ]
            );
            assert_eq!(
                config.trace_propagation_style_extract,
                vec![TracePropagationStyle::Datadog]
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
            let config = get_config(Path::new(""), MOCK_REGION);
            assert!(config.is_ok());
            Ok(())
        });
    }
}
