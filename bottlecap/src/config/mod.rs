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
    let mut config = get_config_with_extension::<LambdaConfig>(config_directory);
    config.ext.apply_experimental_features_gate();
    config
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
    deserialize_optional_string as deser_opt_str,
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
    pub serverless_appsec_enabled: bool,
    pub appsec_rules: Option<String>,
    pub appsec_waf_timeout: Duration,
    pub api_security_enabled: bool,
    pub api_security_sample_delay: Duration,
    pub custom_metrics_exclude_tags: Vec<String>,

    /// When true, inferred (synthetic) event-source spans report the function's
    /// base service (`DD_SERVICE`) instead of the AWS resource/instance
    /// representation. An explicit `DD_SERVICE_MAPPING` entry still wins.
    /// Defaults to `false`.
    pub trace_remove_integration_service_names_enabled: bool,

    /// Maximum number of request IDs whose logs are held in `held_logs` waiting for durable
    /// execution context. Set to 0 to disable log holding; logs will be flushed immediately
    /// without durable execution context enrichment. Holding begins at cold start, before the
    /// extension knows whether the function is durable, and stops for non-durable functions
    /// once `PlatformInitStart` resolves that.
    pub lambda_durable_function_log_buffer_size: usize,

    // Data Streams Monitoring
    /// Enable extension-side DSM consume checkpoints. Gated by the same
    /// `DD_DATA_STREAMS_ENABLED` flag the tracer libraries use; the extension
    /// and tracer never emit checkpoints for the same runtime, so sharing the
    /// flag cannot double-count.
    /// Java/.NET/Go - Datadog Lambda supports calls to '/start-invocation', no tracer support for parsing Lambda payloads
    /// Python - Wrapper script in datadog-lambda-python extracts context and DSM, but does not call `/start-invocation`
    /// JS - Wrapper script in datadog-lambda-js extracts context and DSM, but does not call `/start-invocation`
    pub dsm_consume_enabled: bool,
    /// Fallback DSM `exchange` (event bus name) used for `EventBridge` consume
    /// checkpoints when it cannot be derived from the event payload
    /// (`DD_DSM_EXCHANGE_NAME`).
    pub dsm_exchange_name: Option<String>,
    /// Consumer group used for `MSK`/Kafka DSM consume checkpoints, which is not
    /// present in the Lambda event payload (`DD_DSM_KAFKA_GROUP`).
    pub dsm_kafka_group: Option<String>,

    /// `DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED`: gates `additional_metric_tags` and
    /// `additional_metric_tags_cardinality_limit` below, matching the Serverless
    /// Compatibility Layer (`datadog-trace-agent`).
    pub trace_experimental_features_enabled: bool,
    /// `DD_TRACE_STATS_ADDITIONAL_TAGS`: comma-separated span `meta` keys included as
    /// additional dimensions on trace stats aggregation (`ClientGroupedStats.additional_metric_tags`).
    /// Only honored when `trace_experimental_features_enabled` is true.
    pub additional_metric_tags: Vec<String>,
    /// `DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT`: per-bucket cap on distinct
    /// `additional_metric_tags` value combinations; `None` uses libdatadog's default (100).
    /// Only honored when `trace_experimental_features_enabled` is true.
    pub additional_metric_tags_cardinality_limit: Option<usize>,
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
            serverless_appsec_enabled: false,
            appsec_rules: None,
            appsec_waf_timeout: Duration::from_millis(5),
            api_security_enabled: true,
            api_security_sample_delay: Duration::from_secs(30),
            custom_metrics_exclude_tags: Vec::new(),
            trace_remove_integration_service_names_enabled: false,
            lambda_durable_function_log_buffer_size: 5,
            dsm_consume_enabled: false,
            dsm_exchange_name: None,
            dsm_kafka_group: None,
            trace_experimental_features_enabled: false,
            additional_metric_tags: Vec::new(),
            additional_metric_tags_cardinality_limit: None,
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

    /// `DD_SERVERLESS_LOGS_ENABLED` — Lambda-specific log toggle, kept for
    /// backwards compatibility. Defaults to `true` (the Lambda extension's
    /// historical behavior).
    #[serde(deserialize_with = "deser_opt_bool")]
    pub serverless_logs_enabled: Option<bool>,
    /// `DD_LOGS_ENABLED` — deserialized here a second time (the canonical
    /// `Config::logs_enabled` upstream field is also populated by the upstream
    /// env/yaml parsing) because lambda's default for logs is `true` while
    /// upstream's is `false`. Keeping the alias as `Option<bool>` lets
    /// `merge_from` detect "was it explicitly set?" and OR-merge it into
    /// `serverless_logs_enabled` — that is the field lambda call sites read.
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

    /// `DD_TRACE_REMOVE_INTEGRATION_SERVICE_NAMES_ENABLED` — when true, inferred
    /// (synthetic) event-source spans use `DD_SERVICE` rather than the AWS
    /// resource/instance name. Defaults to `false`.
    #[serde(deserialize_with = "deser_opt_bool")]
    pub trace_remove_integration_service_names_enabled: Option<bool>,

    /// `DD_LAMBDA_DURABLE_FUNCTION_LOG_BUFFER_SIZE` — max number of request IDs
    /// whose logs are held waiting for durable execution context. Defaults to
    /// 5; set to 0 to disable the hold mechanism.
    #[serde(deserialize_with = "deser_opt_lossless")]
    pub lambda_durable_function_log_buffer_size: Option<usize>,

    /// `DD_DATA_STREAMS_ENABLED` — enable extension-side DSM consume
    /// checkpoints. Shared with the tracer libraries; merges into the
    /// `dsm_consume_enabled` config field.
    #[serde(deserialize_with = "deser_opt_bool")]
    pub data_streams_enabled: Option<bool>,
    /// `DD_DSM_EXCHANGE_NAME` — fallback exchange name for `EventBridge` DSM checkpoints.
    #[serde(deserialize_with = "deser_opt_str")]
    pub dsm_exchange_name: Option<String>,
    /// `DD_DSM_KAFKA_GROUP` — consumer group for MSK/Kafka DSM consume checkpoints.
    #[serde(deserialize_with = "deser_opt_str")]
    pub dsm_kafka_group: Option<String>,

    /// `DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED`: see `LambdaConfig::trace_experimental_features_enabled`.
    #[serde(deserialize_with = "deser_opt_bool")]
    pub trace_experimental_features_enabled: Option<bool>,
    /// `DD_TRACE_STATS_ADDITIONAL_TAGS`: see `LambdaConfig::additional_metric_tags`.
    /// Gated after all config sources merge by `apply_experimental_features_gate`. Field is
    /// named `trace_stats_additional_tags` (rather than `additional_metric_tags`) so it maps
    /// to the `DD_TRACE_STATS_ADDITIONAL_TAGS` env var via the field-name-to-env-var convention.
    #[serde(deserialize_with = "deser_csv")]
    pub trace_stats_additional_tags: Vec<String>,
    /// `DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT`: see
    /// `LambdaConfig::additional_metric_tags_cardinality_limit`. Gated after all config sources
    /// merge by `apply_experimental_features_gate`. See
    /// `trace_stats_additional_tags` above for why the field name differs from the
    /// `LambdaConfig` field it merges into.
    #[serde(deserialize_with = "deser_opt_lossless")]
    pub trace_stats_additional_tags_cardinality_limit: Option<usize>,
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
                trace_remove_integration_service_names_enabled,
                lambda_durable_function_log_buffer_size,
                trace_experimental_features_enabled,
            ],
            option: [span_dedup_timeout, api_key_secret_reload_interval, appsec_rules, dsm_exchange_name, dsm_kafka_group],
        );

        // data_streams_enabled (source / DD_DATA_STREAMS_ENABLED) →
        // dsm_consume_enabled (config)
        datadog_agent_config::merge_option_to_value!(
            self,
            dsm_consume_enabled,
            source,
            data_streams_enabled
        );

        // Preserve legacy OR-merge semantics: when either env var is
        // explicitly set, the resolved value is the OR of the two (unset
        // counts as false for the OR). When neither is set, the default
        // (true) is preserved. This invariant — in particular that setting
        // only DD_LOGS_ENABLED=false disables logs — predates upstream
        // owning `logs_enabled` and must be kept. The duplicate parse of
        // DD_LOGS_ENABLED (once upstream, once here via the alias) is
        // intentional: upstream populates `config.logs_enabled` for any
        // non-lambda consumer, while this branch keeps the lambda contract.
        if source.serverless_logs_enabled.is_some() || source.logs_enabled.is_some() {
            self.serverless_logs_enabled = source.serverless_logs_enabled.unwrap_or(false)
                || source.logs_enabled.unwrap_or(false);
        }

        // lambda_customer_metrics_exclude_tags (source) → custom_metrics_exclude_tags (config)
        if !source.lambda_customer_metrics_exclude_tags.is_empty() {
            self.custom_metrics_exclude_tags
                .clone_from(&source.lambda_customer_metrics_exclude_tags);
        }

        // trace_stats_additional_tags (source) → additional_metric_tags (config), and likewise
        // for the cardinality limit. Merged unconditionally here: `merge_from` runs once per
        // config source (datadog.yaml, then env vars), so gating on
        // `trace_experimental_features_enabled` at this point would discard a value read from
        // datadog.yaml whenever the gate itself only arrives with the later env-var pass.
        // `apply_experimental_features_gate` applies the gate once, after every source has
        // merged.
        if !source.trace_stats_additional_tags.is_empty() {
            self.additional_metric_tags
                .clone_from(&source.trace_stats_additional_tags);
        }
        if let Some(limit) = source.trace_stats_additional_tags_cardinality_limit {
            self.additional_metric_tags_cardinality_limit = Some(limit);
        }
    }
}

impl LambdaConfig {
    /// Drop `additional_metric_tags` / `additional_metric_tags_cardinality_limit` unless
    /// `trace_experimental_features_enabled` is set, matching the Serverless Compatibility
    /// Layer (`datadog-trace-agent`).
    ///
    /// Applied after all config sources have merged, not inside `merge_from`, so that the gate
    /// and the values it gates can come from different sources in either order.
    fn apply_experimental_features_gate(&mut self) {
        if !self.trace_experimental_features_enabled {
            self.additional_metric_tags.clear();
            self.additional_metric_tags_cardinality_limit = None;
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod lambda_config_tests {
    use datadog_agent_config::{Config as UpstreamConfig, flush_strategy::PeriodicStrategy};
    use figment::Jail;

    use super::*;

    fn load(
        jail_setup: impl FnOnce(&mut Jail) -> figment::Result<()>,
    ) -> UpstreamConfig<LambdaConfig> {
        let mut result: Option<UpstreamConfig<LambdaConfig>> = None;
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail_setup(jail)?;
            // `get_config`, not `get_config_with_extension`, so the post-merge
            // `apply_experimental_features_gate` step is covered too.
            result = Some(get_config(Path::new("")));
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
    //
    // The legacy contract: DD_SERVERLESS_LOGS_ENABLED and DD_LOGS_ENABLED are
    // OR-merged into config.ext.serverless_logs_enabled. The default (true)
    // is preserved iff neither env var was explicitly set; otherwise the
    // resolved value is the OR of the two (unset counts as false). The
    // upstream `Config::logs_enabled` field is also populated independently
    // (sourced by the upstream env/yaml parsing), but lambda call sites that
    // gate on log shipping continue to use serverless_logs_enabled.

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
    fn logs_enabled_alias_only_true() {
        let config = load(|jail| {
            jail.set_env("DD_LOGS_ENABLED", "true");
            Ok(())
        });
        assert!(config.ext.serverless_logs_enabled);
    }

    #[test]
    fn logs_enabled_alias_only_false_overrides_default() {
        // Setting only DD_LOGS_ENABLED=false must disable logs, overriding
        // the default-true. This is the legacy behavior that the alias-only
        // entry into the OR-merge guards.
        let config = load(|jail| {
            jail.set_env("DD_LOGS_ENABLED", "false");
            Ok(())
        });
        assert!(!config.ext.serverless_logs_enabled);
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

    #[test]
    fn dd_logs_enabled_also_populates_upstream_field() {
        // The upstream Config::logs_enabled field is wired through the
        // upstream env parsing independently of the lambda alias. Lambda
        // doesn't read this field, but other consumers of the same crate do.
        let config = load(|jail| {
            jail.set_env("DD_LOGS_ENABLED", "true");
            Ok(())
        });
        assert!(config.logs_enabled);
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

    #[test]
    fn trace_remove_integration_service_names_defaults_false() {
        let config = load(|_| Ok(()));
        assert!(!config.ext.trace_remove_integration_service_names_enabled);
    }

    #[test]
    fn trace_remove_integration_service_names_from_env() {
        let config = load(|jail| {
            jail.set_env("DD_TRACE_REMOVE_INTEGRATION_SERVICE_NAMES_ENABLED", "true");
            Ok(())
        });
        assert!(config.ext.trace_remove_integration_service_names_enabled);
    }

    #[test]
    fn trace_remove_integration_service_names_from_yaml() {
        let config = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "trace_remove_integration_service_names_enabled: true\n",
            )?;
            Ok(())
        });
        assert!(config.ext.trace_remove_integration_service_names_enabled);
    }

    #[test]
    fn dsm_consume_enabled_from_data_streams_env() {
        let config = load(|jail| {
            jail.set_env("DD_DATA_STREAMS_ENABLED", "true");
            Ok(())
        });
        assert!(config.ext.dsm_consume_enabled);
    }

    #[test]
    fn dsm_consume_enabled_defaults_false() {
        let config = load(|_| Ok(()));
        assert!(!config.ext.dsm_consume_enabled);
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
        assert_eq!(config.dd_org_uuid, "00000000-1111-2222-3333-444444444444");
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
        assert_eq!(config.dd_org_uuid, "00000000-1111-2222-3333-444444444444");
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

    // ---- additional_metric_tags (span-derived primary tags), gated on
    // trace_experimental_features_enabled, matching the Serverless Compatibility Layer
    // (datadog-trace-agent) ----

    #[test]
    fn additional_metric_tags_ignored_when_experimental_features_disabled() {
        let config = load(|jail| {
            jail.set_env("DD_TRACE_STATS_ADDITIONAL_TAGS", "region,tenant_id");
            Ok(())
        });
        assert!(!config.ext.trace_experimental_features_enabled);
        assert!(config.ext.additional_metric_tags.is_empty());
    }

    #[test]
    fn additional_metric_tags_from_env_when_trace_experimental_features_enabled() {
        let config = load(|jail| {
            jail.set_env("DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED", "true");
            jail.set_env("DD_TRACE_STATS_ADDITIONAL_TAGS", "region, tenant_id");
            Ok(())
        });
        assert!(config.ext.trace_experimental_features_enabled);
        assert_eq!(
            config.ext.additional_metric_tags,
            vec!["region".to_string(), "tenant_id".to_string()]
        );
    }

    #[test]
    fn additional_metric_tags_cardinality_limit_ignored_when_experimental_features_disabled() {
        let config = load(|jail| {
            jail.set_env("DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT", "5");
            Ok(())
        });
        assert_eq!(config.ext.additional_metric_tags_cardinality_limit, None);
    }

    #[test]
    fn additional_metric_tags_cardinality_limit_from_env_when_experimental_gate_enabled() {
        let config = load(|jail| {
            jail.set_env("DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED", "true");
            jail.set_env("DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT", "5");
            Ok(())
        });
        assert_eq!(config.ext.additional_metric_tags_cardinality_limit, Some(5));
    }

    #[test]
    fn additional_metric_tags_cardinality_limit_invalid_value_falls_back_to_none() {
        let config = load(|jail| {
            jail.set_env("DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED", "true");
            jail.set_env(
                "DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT",
                "not-a-number",
            );
            Ok(())
        });
        assert_eq!(config.ext.additional_metric_tags_cardinality_limit, None);
    }

    /// The gate and the values it gates may come from different config sources. Sources merge
    /// one at a time (datadog.yaml first, then env vars), so gating during the merge would
    /// drop the yaml values before the env-var pass ever enables the gate.
    #[test]
    fn additional_metric_tags_from_yaml_survive_an_env_only_experimental_gate() {
        let config = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "trace_stats_additional_tags: \"region,zone\"\n\
                 trace_stats_additional_tags_cardinality_limit: 7\n",
            )?;
            jail.set_env("DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED", "true");
            Ok(())
        });
        assert_eq!(
            config.ext.additional_metric_tags,
            vec!["region".to_string(), "zone".to_string()]
        );
        assert_eq!(config.ext.additional_metric_tags_cardinality_limit, Some(7));
    }

    /// The mirror of the above: an env-var gate of `false` must still win over yaml values.
    #[test]
    fn additional_metric_tags_from_yaml_dropped_when_env_disables_the_gate() {
        let config = load(|jail| {
            jail.create_file(
                "datadog.yaml",
                "trace_experimental_features_enabled: true\n\
                 trace_stats_additional_tags: \"region,zone\"\n\
                 trace_stats_additional_tags_cardinality_limit: 7\n",
            )?;
            jail.set_env("DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED", "false");
            Ok(())
        });
        assert!(config.ext.additional_metric_tags.is_empty());
        assert_eq!(config.ext.additional_metric_tags_cardinality_limit, None);
    }
}
