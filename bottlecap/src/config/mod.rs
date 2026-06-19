pub mod aws;
pub mod propagation_wrapper;

// Re-export upstream config submodules so existing `crate::config::env::*`,
// `crate::config::flush_strategy::*`, etc. imports across bottlecap keep
// working without forcing every consumer to switch to the upstream path.
pub use datadog_agent_config::{
    TracePropagationStyle, additional_endpoints, apm_replace_rule, deserialize_apm_filter_tags,
    deserialize_array_from_comma_separated_string, deserialize_key_value_pair_array_to_hashmap,
    deserialize_key_value_pairs, deserialize_option_lossless,
    deserialize_optional_bool_from_anything, deserialize_optional_duration_from_microseconds,
    deserialize_optional_duration_from_seconds,
    deserialize_optional_duration_from_seconds_ignore_zero, deserialize_optional_string,
    deserialize_string_or_int, env, flush_strategy, get_config_with_extension, log_level,
    logs_additional_endpoints, processing_rule, service_mapping, yaml,
};

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

/// Bottlecap's resolved configuration: the shared agent core plus a Lambda
/// extension under `.ext`.
pub type Config = datadog_agent_config::Config<LambdaConfig>;

#[allow(clippy::module_name_repetitions)]
#[inline]
#[must_use]
pub fn get_config(config_directory: &Path) -> Config {
    get_config_with_extension::<LambdaConfig>(config_directory)
}
// ---------------------------------------------------------------------------
// LambdaConfig — bottlecap's `ConfigExtension` for the shared
// `datadog-agent-config` crate. Lives alongside the core config under
// `Config::ext` once the migration onto upstream lands; see the migration PR
// description for the full plan.
// ---------------------------------------------------------------------------

use datadog_agent_config::{
    ConfigExtension as DatadogConfigExtension,
    deserialize_array_from_comma_separated_string as deser_csv,
    deserialize_option_lossless as deser_opt_lossless,
    deserialize_optional_bool_from_anything as deser_opt_bool,
    deserialize_optional_duration_from_microseconds as deser_dur_micros,
    deserialize_optional_duration_from_seconds as deser_dur_secs,
    deserialize_optional_duration_from_seconds_ignore_zero as deser_dur_secs_ignore_zero,
    deserialize_optional_string as deser_opt_str, deserialize_string_or_int as deser_str_or_int,
    flush_strategy::FlushStrategy as UpstreamFlushStrategy,
};

/// Lambda-specific configuration that lives alongside the shared
/// `datadog_agent_config::Config` core fields under `config.ext` once the
/// migration onto upstream lands.
#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::module_name_repetitions)]
#[allow(clippy::struct_excessive_bools)]
pub struct LambdaConfig {
    pub api_key_secret_arn: String,
    pub kms_api_key: String,
    pub api_key_ssm_arn: String,
    pub serverless_logs_enabled: bool,
    pub serverless_flush_strategy: UpstreamFlushStrategy,
    pub enhanced_metrics: bool,
    pub lambda_proc_enhanced_metrics: bool,
    pub capture_lambda_payload: bool,
    pub capture_lambda_payload_max_depth: u32,
    pub lambda_extension_compute_stats: bool,
    pub span_dedup_timeout: Option<Duration>,
    pub api_key_secret_reload_interval: Option<Duration>,
    pub dd_org_uuid: String,
    pub serverless_appsec_enabled: bool,
    pub appsec_rules: Option<String>,
    pub appsec_waf_timeout: Duration,
    pub api_security_enabled: bool,
    pub api_security_sample_delay: Duration,
    pub custom_metrics_exclude_tags: Vec<String>,

    /// Maximum number of request IDs whose logs are held in `held_logs` waiting for durable
    /// execution context. Set to 0 to disable log holding; logs will be flushed immediately
    /// without durable execution context enrichment. Defaults to 0 until the tracer-side
    /// durable execution support is released; set to 50 to re-enable enrichment.
    pub lambda_durable_function_log_buffer_size: usize,

    // Data Streams Monitoring
    /// Enable extension-side DSM consume checkpoints (`DD_DSM_CONSUME_ENABLED`).
    pub dsm_consume_enabled: bool,
    /// Fallback DSM `exchange` (event bus name) used for `EventBridge` consume
    /// checkpoints when it cannot be derived from the event payload
    /// (`DD_DSM_EXCHANGE_NAME`).
    pub dsm_exchange_name: Option<String>,
    /// Consumer group used for `MSK`/Kafka DSM consume checkpoints, which is not
    /// present in the Lambda event payload (`DD_DSM_KAFKA_GROUP`).
    pub dsm_kafka_group: Option<String>,
}

impl Default for LambdaConfig {
    fn default() -> Self {
        Self {
            api_key_secret_arn: String::new(),
            kms_api_key: String::new(),
            api_key_ssm_arn: String::new(),
            serverless_logs_enabled: true,
            serverless_flush_strategy: UpstreamFlushStrategy::Default,
            enhanced_metrics: true,
            lambda_proc_enhanced_metrics: true,
            capture_lambda_payload: false,
            capture_lambda_payload_max_depth: 10,
            lambda_extension_compute_stats: false,
            span_dedup_timeout: None,
            api_key_secret_reload_interval: None,
            dd_org_uuid: String::new(),
            serverless_appsec_enabled: false,
            appsec_rules: None,
            appsec_waf_timeout: Duration::from_millis(5),
            api_security_enabled: true,
            api_security_sample_delay: Duration::from_secs(30),
            custom_metrics_exclude_tags: Vec::new(),
            lambda_durable_function_log_buffer_size: 0,
            dsm_consume_enabled: false,
            dsm_exchange_name: None,
            dsm_kafka_group: None,
        }
    }
}

/// Intermediate deserialization type shared by env-var and YAML loading.
///
/// `#[serde(default)]` and the forgiving per-field deserializers are required
/// by the `ConfigExtension` contract: one malformed field must not fail the
/// whole extraction.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct LambdaConfigSource {
    #[serde(deserialize_with = "deser_opt_str")]
    pub api_key_secret_arn: Option<String>,
    #[serde(deserialize_with = "deser_opt_str")]
    pub kms_api_key: Option<String>,
    #[serde(deserialize_with = "deser_opt_str")]
    pub api_key_ssm_arn: Option<String>,

    /// `DD_SERVERLESS_LOGS_ENABLED` — primary toggle for Lambda log shipping.
    #[serde(deserialize_with = "deser_opt_bool")]
    pub serverless_logs_enabled: Option<bool>,
    /// `DD_LOGS_ENABLED` — alias for `serverless_logs_enabled`; OR-merged so
    /// either being `true` turns logs on. See `merge_from` below.
    #[serde(deserialize_with = "deser_opt_bool")]
    pub logs_enabled: Option<bool>,

    pub serverless_flush_strategy: Option<UpstreamFlushStrategy>,

    #[serde(deserialize_with = "deser_opt_bool")]
    pub enhanced_metrics: Option<bool>,
    #[serde(deserialize_with = "deser_opt_bool")]
    pub lambda_proc_enhanced_metrics: Option<bool>,
    #[serde(deserialize_with = "deser_opt_bool")]
    pub capture_lambda_payload: Option<bool>,
    #[serde(deserialize_with = "deser_opt_lossless")]
    pub capture_lambda_payload_max_depth: Option<u32>,
    /// `DD_LAMBDA_EXTENSION_COMPUTE_STATS` — when true, the extension computes
    /// APM trace stats locally instead of letting the backend do it.
    #[serde(deserialize_with = "deser_opt_bool")]
    pub lambda_extension_compute_stats: Option<bool>,

    #[serde(deserialize_with = "deser_dur_secs_ignore_zero")]
    pub span_dedup_timeout: Option<Duration>,
    #[serde(deserialize_with = "deser_dur_secs_ignore_zero")]
    pub api_key_secret_reload_interval: Option<Duration>,

    /// `DD_ORG_UUID` — when set, delegated auth is auto-enabled. The source
    /// field is `org_uuid` (matching the env var) and merges into the
    /// `dd_org_uuid` config field.
    #[serde(deserialize_with = "deser_str_or_int")]
    pub org_uuid: Option<String>,

    #[serde(deserialize_with = "deser_opt_bool")]
    pub serverless_appsec_enabled: Option<bool>,
    #[serde(deserialize_with = "deser_opt_str")]
    pub appsec_rules: Option<String>,
    #[serde(deserialize_with = "deser_dur_micros")]
    pub appsec_waf_timeout: Option<Duration>,
    #[serde(deserialize_with = "deser_opt_bool")]
    pub api_security_enabled: Option<bool>,
    #[serde(deserialize_with = "deser_dur_secs")]
    pub api_security_sample_delay: Option<Duration>,

    /// `DD_LAMBDA_CUSTOMER_METRICS_EXCLUDE_TAGS` — comma-separated list of tag
    /// names to drop from customer `DogStatsD` metrics. Source field name
    /// matches the env var; merges into `custom_metrics_exclude_tags`.
    #[serde(deserialize_with = "deser_csv")]
    pub lambda_customer_metrics_exclude_tags: Vec<String>,

    /// `DD_LAMBDA_DURABLE_FUNCTION_LOG_BUFFER_SIZE` — max number of request IDs
    /// whose logs are held waiting for durable execution context. Defaults to
    /// 0 (hold mechanism disabled).
    #[serde(deserialize_with = "deser_opt_lossless")]
    pub lambda_durable_function_log_buffer_size: Option<usize>,

    /// `DD_DSM_CONSUME_ENABLED` — enable extension-side DSM consume checkpoints.
    #[serde(deserialize_with = "deser_opt_bool")]
    pub dsm_consume_enabled: Option<bool>,
    /// `DD_DSM_EXCHANGE_NAME` — fallback exchange name for EventBridge DSM checkpoints.
    #[serde(deserialize_with = "deser_opt_str")]
    pub dsm_exchange_name: Option<String>,
    /// `DD_DSM_KAFKA_GROUP` — consumer group for MSK/Kafka DSM consume checkpoints.
    #[serde(deserialize_with = "deser_opt_str")]
    pub dsm_kafka_group: Option<String>,
}

impl DatadogConfigExtension for LambdaConfig {
    type Source = LambdaConfigSource;

    fn merge_from(&mut self, source: &Self::Source) {
        // Fully-qualified macro paths avoid colliding with the legacy
        // `merge_*` macros declared with `#[macro_export]` at the top of this
        // file, which will be removed once the migration onto upstream is
        // complete.
        datadog_agent_config::merge_fields!(self, source,
            string: [api_key_secret_arn, kms_api_key, api_key_ssm_arn],
            value:  [
                serverless_flush_strategy,
                enhanced_metrics,
                lambda_proc_enhanced_metrics,
                capture_lambda_payload,
                capture_lambda_payload_max_depth,
                lambda_extension_compute_stats,
                serverless_appsec_enabled,
                appsec_waf_timeout,
                api_security_enabled,
                api_security_sample_delay,
                lambda_durable_function_log_buffer_size,
            ],
            option: [span_dedup_timeout, api_key_secret_reload_interval, appsec_rules, dsm_exchange_name, dsm_kafka_group],
        );

        datadog_agent_config::merge_fields!(self, source,
            value: [dsm_consume_enabled],
        );

        // OR-merge serverless_logs_enabled with the logs_enabled alias. Either
        // env var set to `true` enables logs; if both are absent the default
        // (true) is preserved.
        if source.serverless_logs_enabled.is_some() || source.logs_enabled.is_some() {
            self.serverless_logs_enabled = source.serverless_logs_enabled.unwrap_or(false)
                || source.logs_enabled.unwrap_or(false);
        }

        // org_uuid (source) → dd_org_uuid (config)
        datadog_agent_config::merge_string!(self, dd_org_uuid, source, org_uuid);

        // lambda_customer_metrics_exclude_tags (source) → custom_metrics_exclude_tags (config)
        if !source.lambda_customer_metrics_exclude_tags.is_empty() {
            self.custom_metrics_exclude_tags
                .clone_from(&source.lambda_customer_metrics_exclude_tags);
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod lambda_config_tests {
    use datadog_agent_config::{
        Config as UpstreamConfig, flush_strategy::PeriodicStrategy, get_config_with_extension,
    };
    use figment::Jail;

    use super::*;

    fn load(
        jail_setup: impl FnOnce(&mut Jail) -> figment::Result<()>,
    ) -> UpstreamConfig<LambdaConfig> {
        let mut result: Option<UpstreamConfig<LambdaConfig>> = None;
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail_setup(jail)?;
            result = Some(get_config_with_extension::<LambdaConfig>(Path::new("")));
            Ok(())
        });
        result.unwrap()
    }

    #[test]
    fn defaults_match_lambda_config_default() {
        let config = load(|_| Ok(()));
        assert_eq!(config.ext, LambdaConfig::default());
    }

    // ---- string fields from env / yaml ----

    #[test]
    fn api_key_secret_arn_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_API_KEY_SECRET_ARN", "arn:aws:secretsmanager:foo");
            Ok(())
        });
        assert_eq!(config.ext.api_key_secret_arn, "arn:aws:secretsmanager:foo");
    }

    #[test]
    fn api_key_secret_arn_from_yaml() {
        let config = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "api_key_secret_arn: arn:aws:secretsmanager:foo\n",
            )?;
            Ok(())
        });
        assert_eq!(config.ext.api_key_secret_arn, "arn:aws:secretsmanager:foo");
    }

    #[test]
    fn kms_api_key_from_env_and_yaml() {
        let env = load(|jail| {
            jail.set_env("DD_KMS_API_KEY", "kms-key-env");
            Ok(())
        });
        assert_eq!(env.ext.kms_api_key, "kms-key-env");

        let yaml = load(|jail| {
            jail.create_file("datadog.yaml", "kms_api_key: kms-key-yaml\n")?;
            Ok(())
        });
        assert_eq!(yaml.ext.kms_api_key, "kms-key-yaml");
    }

    #[test]
    fn api_key_ssm_arn_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_API_KEY_SSM_ARN", "ssm-arn");
            Ok(())
        });
        assert_eq!(config.ext.api_key_ssm_arn, "ssm-arn");
    }

    #[test]
    fn api_key_ssm_arn_from_yaml() {
        let config = load(|jail| {
            jail.create_file("datadog.yaml", "api_key_ssm_arn: ssm-yaml\n")?;
            Ok(())
        });
        assert_eq!(config.ext.api_key_ssm_arn, "ssm-yaml");
    }

    // ---- serverless_logs_enabled with OR-merge alias ----

    #[test]
    fn serverless_logs_enabled_defaults_true() {
        let config = load(|_| Ok(()));
        assert!(config.ext.serverless_logs_enabled);
    }

    #[test]
    fn serverless_logs_enabled_false_explicit() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_LOGS_ENABLED", "false");
            Ok(())
        });
        assert!(!config.ext.serverless_logs_enabled);
    }

    #[test]
    fn logs_enabled_alias_turns_on_when_serverless_is_off() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_LOGS_ENABLED", "false");
            jail.set_env("DD_LOGS_ENABLED", "true");
            Ok(())
        });
        assert!(config.ext.serverless_logs_enabled);
    }

    #[test]
    fn logs_enabled_alias_only() {
        let config = load(|jail| {
            jail.set_env("DD_LOGS_ENABLED", "true");
            Ok(())
        });
        assert!(config.ext.serverless_logs_enabled);
    }

    #[test]
    fn serverless_logs_disabled_when_both_false() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_LOGS_ENABLED", "false");
            jail.set_env("DD_LOGS_ENABLED", "false");
            Ok(())
        });
        assert!(!config.ext.serverless_logs_enabled);
    }

    #[test]
    fn serverless_logs_enabled_from_yaml() {
        let config = load(|jail| {
            jail.create_file("datadog.yaml", "serverless_logs_enabled: false\n")?;
            Ok(())
        });
        assert!(!config.ext.serverless_logs_enabled);
    }

    // ---- FlushStrategy ----

    #[test]
    fn flush_strategy_end_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "end");
            Ok(())
        });
        assert_eq!(
            config.ext.serverless_flush_strategy,
            UpstreamFlushStrategy::End
        );
    }

    #[test]
    fn flush_strategy_periodically_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "periodically,60000");
            Ok(())
        });
        assert_eq!(
            config.ext.serverless_flush_strategy,
            UpstreamFlushStrategy::Periodically(PeriodicStrategy { interval: 60000 })
        );
    }

    #[test]
    fn flush_strategy_periodically_from_yaml() {
        let config = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "serverless_flush_strategy: \"periodically,5000\"\n",
            )?;
            Ok(())
        });
        assert_eq!(
            config.ext.serverless_flush_strategy,
            UpstreamFlushStrategy::Periodically(PeriodicStrategy { interval: 5000 })
        );
    }

    #[test]
    fn flush_strategy_invalid_falls_back_to_default() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "garbage");
            Ok(())
        });
        assert_eq!(
            config.ext.serverless_flush_strategy,
            UpstreamFlushStrategy::Default
        );
    }

    #[test]
    fn flush_strategy_end_periodically_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "end,1000");
            Ok(())
        });
        assert_eq!(
            config.ext.serverless_flush_strategy,
            UpstreamFlushStrategy::EndPeriodically(PeriodicStrategy { interval: 1000 })
        );
    }

    #[test]
    fn flush_strategy_continuously_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "continuously,2000");
            Ok(())
        });
        assert_eq!(
            config.ext.serverless_flush_strategy,
            UpstreamFlushStrategy::Continuously(PeriodicStrategy { interval: 2000 })
        );
    }

    // ---- bool fields ----

    #[test]
    fn enhanced_metrics_disabled_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_ENHANCED_METRICS", "false");
            Ok(())
        });
        assert!(!config.ext.enhanced_metrics);
    }

    #[test]
    fn lambda_proc_enhanced_metrics_disabled_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_LAMBDA_PROC_ENHANCED_METRICS", "false");
            Ok(())
        });
        assert!(!config.ext.lambda_proc_enhanced_metrics);
    }

    #[test]
    fn capture_lambda_payload_from_env_and_yaml() {
        let env = load(|jail| {
            jail.set_env("DD_CAPTURE_LAMBDA_PAYLOAD", "true");
            jail.set_env("DD_CAPTURE_LAMBDA_PAYLOAD_MAX_DEPTH", "5");
            Ok(())
        });
        assert!(env.ext.capture_lambda_payload);
        assert_eq!(env.ext.capture_lambda_payload_max_depth, 5);

        let yaml = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "capture_lambda_payload: true\ncapture_lambda_payload_max_depth: 3\n",
            )?;
            Ok(())
        });
        assert!(yaml.ext.capture_lambda_payload);
        assert_eq!(yaml.ext.capture_lambda_payload_max_depth, 3);
    }

    #[test]
    fn lambda_extension_compute_stats_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_LAMBDA_EXTENSION_COMPUTE_STATS", "true");
            Ok(())
        });
        assert!(config.ext.lambda_extension_compute_stats);
    }

    #[test]
    fn lambda_extension_compute_stats_defaults_false() {
        let config = load(|_| Ok(()));
        assert!(!config.ext.lambda_extension_compute_stats);
    }

    // ---- Duration fields ----

    #[test]
    fn span_dedup_timeout_from_env_seconds() {
        let config = load(|jail| {
            jail.set_env("DD_SPAN_DEDUP_TIMEOUT", "5");
            Ok(())
        });
        assert_eq!(config.ext.span_dedup_timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn span_dedup_timeout_zero_treated_as_none() {
        let config = load(|jail| {
            jail.set_env("DD_SPAN_DEDUP_TIMEOUT", "0");
            Ok(())
        });
        assert_eq!(config.ext.span_dedup_timeout, None);
    }

    #[test]
    fn api_key_secret_reload_interval_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_API_KEY_SECRET_RELOAD_INTERVAL", "10");
            Ok(())
        });
        assert_eq!(
            config.ext.api_key_secret_reload_interval,
            Some(Duration::from_secs(10))
        );
    }

    #[test]
    fn appsec_waf_timeout_from_env_microseconds() {
        let config = load(|jail| {
            jail.set_env("DD_APPSEC_WAF_TIMEOUT", "1000000");
            Ok(())
        });
        assert_eq!(config.ext.appsec_waf_timeout, Duration::from_secs(1));
    }

    #[test]
    fn appsec_waf_timeout_from_yaml() {
        let config = load(|jail| {
            jail.create_file("datadog.yaml", "appsec_waf_timeout: 1000000\n")?;
            Ok(())
        });
        assert_eq!(config.ext.appsec_waf_timeout, Duration::from_secs(1));
    }

    #[test]
    fn api_security_sample_delay_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_API_SECURITY_SAMPLE_DELAY", "60");
            Ok(())
        });
        assert_eq!(
            config.ext.api_security_sample_delay,
            Duration::from_secs(60)
        );
    }

    // ---- AppSec / API Security ----

    #[test]
    fn appsec_block_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_SERVERLESS_APPSEC_ENABLED", "true");
            jail.set_env("DD_APPSEC_RULES", "/etc/dd/rules.json");
            Ok(())
        });
        assert!(config.ext.serverless_appsec_enabled);
        assert_eq!(
            config.ext.appsec_rules.as_deref(),
            Some("/etc/dd/rules.json")
        );
    }

    #[test]
    fn api_security_disabled_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_API_SECURITY_ENABLED", "false");
            Ok(())
        });
        assert!(!config.ext.api_security_enabled);
    }

    // ---- aliased name mappings ----

    #[test]
    fn org_uuid_env_maps_to_dd_org_uuid_field() {
        let config = load(|jail| {
            jail.set_env("DD_ORG_UUID", "00000000-1111-2222-3333-444444444444");
            Ok(())
        });
        assert_eq!(
            config.ext.dd_org_uuid,
            "00000000-1111-2222-3333-444444444444"
        );
    }

    #[test]
    fn org_uuid_yaml_maps_to_dd_org_uuid_field() {
        // The yaml key matches the env-var name minus the DD_ prefix
        // (`org_uuid:`), not the config field name (`dd_org_uuid:`).
        let config = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "org_uuid: 00000000-1111-2222-3333-444444444444\n",
            )?;
            Ok(())
        });
        assert_eq!(
            config.ext.dd_org_uuid,
            "00000000-1111-2222-3333-444444444444"
        );
    }

    #[test]
    fn custom_metrics_exclude_tags_from_env() {
        let config = load(|jail| {
            jail.set_env(
                "DD_LAMBDA_CUSTOMER_METRICS_EXCLUDE_TAGS",
                "function_arn,region",
            );
            Ok(())
        });
        assert_eq!(
            config.ext.custom_metrics_exclude_tags,
            vec!["function_arn".to_string(), "region".to_string()]
        );
    }

    #[test]
    fn custom_metrics_exclude_tags_from_yaml() {
        // YAML key matches the env var name; merges into the
        // `custom_metrics_exclude_tags` config field.
        let config = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "lambda_customer_metrics_exclude_tags: \"function_arn,region\"\n",
            )?;
            Ok(())
        });
        assert_eq!(
            config.ext.custom_metrics_exclude_tags,
            vec!["function_arn".to_string(), "region".to_string()]
        );
    }

    #[test]
    fn custom_metrics_exclude_tags_defaults_to_empty() {
        let config = load(|_| Ok(()));
        assert!(config.ext.custom_metrics_exclude_tags.is_empty());
    }

    // ---- precedence: env wins over yaml for the same field ----

    #[test]
    fn env_overrides_yaml_for_extension_field() {
        let config = load(|jail| {
            jail.create_file("datadog.yaml", "capture_lambda_payload: false\n")?;
            jail.set_env("DD_CAPTURE_LAMBDA_PAYLOAD", "true");
            Ok(())
        });
        assert!(config.ext.capture_lambda_payload);
    }

    // ---- malformed input falls back to default (forgiving deserializers) ----

    #[test]
    fn malformed_bool_falls_back_to_default() {
        let config = load(|jail| {
            jail.set_env("DD_ENHANCED_METRICS", "not-a-bool");
            Ok(())
        });
        // Default is true.
        assert!(config.ext.enhanced_metrics);
    }
}
