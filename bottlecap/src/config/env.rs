use figment::{Figment, providers::Env};
use serde::Deserialize;
use std::collections::HashMap;

use datadog_trace_obfuscation::replacer::ReplaceRule;

use crate::{
    config::{
        Config, ConfigError, ConfigSource,
        additional_endpoints::deserialize_additional_endpoints,
        apm_replace_rule::deserialize_apm_replace_rules,
        deserialize_array_from_comma_separated_string, deserialize_key_value_pairs,
        deserialize_optional_bool_from_anything, deserialize_string_or_int,
        flush_strategy::FlushStrategy,
        log_level::LogLevel,
        logs_additional_endpoints::{
            LogsAdditionalEndpoint, deserialize_logs_additional_endpoints,
        },
        processing_rule::{ProcessingRule, deserialize_processing_rules},
        service_mapping::deserialize_service_mapping,
        trace_propagation_style::{TracePropagationStyle, deserialize_trace_propagation_style},
    },
    merge_hashmap, merge_option, merge_option_to_value, merge_string, merge_vec,
};

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
#[allow(clippy::module_name_repetitions)]
pub struct EnvConfig {
    /// @env `DD_SITE`
    ///
    /// The Datadog site to send telemetry to
    pub site: Option<String>,
    /// @env `DD_API_KEY`
    ///
    /// The Datadog API key used to submit telemetry to Datadog
    pub api_key: Option<String>,
    /// @env `DD_LOG_LEVEL`
    ///
    /// Minimum log level of the Datadog Agent.
    /// Valid log levels are: trace, debug, info, warn, and error.
    pub log_level: Option<LogLevel>,

    /// @env `DD_FLUSH_TIMEOUT`
    ///
    /// Flush timeout in seconds
    /// todo(duncanista): find out where this comes from
    /// todo(?): go agent adds jitter too
    pub flush_timeout: Option<u64>,

    // Proxy
    /// @env `DD_PROXY_HTTPS`
    ///
    /// Proxy endpoint for HTTPS connections (most Datadog traffic)
    pub proxy_https: Option<String>,
    /// @env `DD_PROXY_NO_PROXY`
    ///
    /// Specify hosts the Agent should connect to directly, bypassing the proxy.
    #[serde(deserialize_with = "deserialize_array_from_comma_separated_string")]
    pub proxy_no_proxy: Vec<String>,
    /// @env `DD_HTTP_PROTOCOL`
    ///
    /// The HTTP protocol to use for the Datadog Agent.
    /// The transport type to use for sending logs. Possible values are "auto" or "http1".
    pub http_protocol: Option<String>,

    // Metrics
    /// @env `DD_DD_URL`
    ///
    /// @default `https://app.datadoghq.com`
    ///
    /// The host of the Datadog intake server to send **metrics** to, only set this option
    /// if you need the Agent to send **metrics** to a custom URL, it overrides the site
    /// setting defined in "site". It does not affect APM, Logs, Remote Configuration,
    /// or Live Process intake which have their own "*_`dd_url`" settings.
    ///
    /// If `DD_DD_URL` and `DD_URL` are both set, `DD_DD_URL` is used in priority.
    pub dd_url: Option<String>,
    /// @env `DD_URL`
    ///
    /// @default `https://app.datadoghq.com`
    pub url: Option<String>,
    /// @env `DD_ADDITIONAL_ENDPOINTS`
    ///
    /// Additional endpoints to send metrics to.
    /// <https://docs.datadoghq.com/agent/configuration/dual-shipping/?tab=helm#environment-variable-configuration>
    #[serde(deserialize_with = "deserialize_additional_endpoints")]
    pub additional_endpoints: HashMap<String, Vec<String>>,

    // Unified Service Tagging
    /// @env `DD_ENV`
    ///
    /// The environment name where the agent is running. Attached in-app to every
    /// metric, event, log, trace, and service check emitted by this Agent.
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub env: Option<String>,
    /// @env `DD_SERVICE`
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub service: Option<String>,
    /// @env `DD_VERSION`
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub version: Option<String>,
    /// @env `DD_TAGS`
    #[serde(deserialize_with = "deserialize_key_value_pairs")]
    pub tags: HashMap<String, String>,

    // Logs
    /// @env `DD_LOGS_CONFIG_LOGS_DD_URL`
    ///
    /// Define the endpoint and port to hit when using a proxy for logs.
    pub logs_config_logs_dd_url: Option<String>,
    /// @env `DD_LOGS_CONFIG_PROCESSING_RULES`
    ///
    /// Global processing rules that are applied to all logs. The available rules are
    /// "`exclude_at_match`", "`include_at_match`" and "`mask_sequences`". More information in Datadog documentation:
    /// <https://docs.datadoghq.com/agent/logs/advanced_log_collection/#global-processing-rules>
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub logs_config_processing_rules: Option<Vec<ProcessingRule>>,
    /// @env `DD_LOGS_CONFIG_USE_COMPRESSION`
    ///
    /// If enabled, the Agent compresses logs before sending them.
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub logs_config_use_compression: Option<bool>,
    /// @env `DD_LOGS_CONFIG_COMPRESSION_LEVEL`
    ///
    /// The `compression_level` parameter accepts values from 0 (no compression)
    /// to 9 (maximum compression but higher resource usage). Only takes effect if
    /// `use_compression` is set to `true`.
    pub logs_config_compression_level: Option<i32>,
    /// @env `DD_LOGS_CONFIG_ADDITIONAL_ENDPOINTS`
    ///
    /// Additional endpoints to send logs to.
    /// <https://docs.datadoghq.com/agent/configuration/dual-shipping/?tab=helm#environment-variable-configuration-6>
    #[serde(deserialize_with = "deserialize_logs_additional_endpoints")]
    pub logs_config_additional_endpoints: Vec<LogsAdditionalEndpoint>,

    // APM
    //
    /// @env `DD_SERVICE_MAPPING`
    #[serde(deserialize_with = "deserialize_service_mapping")]
    pub service_mapping: HashMap<String, String>,
    //
    /// @env `DD_APM_DD_URL`
    ///
    /// Define the endpoint and port to hit when using a proxy for APM.
    pub apm_dd_url: Option<String>,
    /// @env `DD_APM_REPLACE_TAGS`
    ///
    /// Defines a set of rules to replace or remove certain resources, tags containing
    /// potentially sensitive information.
    /// Each rule has to contain:
    ///  * name - string - The tag name to replace, for resources use "resource.name".
    ///  * pattern - string - The pattern to match the desired content to replace
    ///  * repl - string - what to inline if the pattern is matched
    ///
    /// <https://docs.datadoghq.com/tracing/setup_overview/configure_data_security/#replace-rules-for-tag-filtering>
    #[serde(deserialize_with = "deserialize_apm_replace_rules")]
    pub apm_replace_tags: Option<Vec<ReplaceRule>>,
    /// @env `DD_APM_CONFIG_OBFUSCATION_HTTP_REMOVE_QUERY_STRING`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub apm_config_obfuscation_http_remove_query_string: Option<bool>,
    /// @env `DD_APM_CONFIG_OBFUSCATION_HTTP_REMOVE_PATHS_WITH_DIGITS`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub apm_config_obfuscation_http_remove_paths_with_digits: Option<bool>,
    /// @env `DD_APM_CONFIG_COMPRESSION_LEVEL`
    ///
    /// The Agent compresses traces before sending them. The `compression_level` parameter
    /// accepts values from 0 (no compression) to 9 (maximum compression but
    /// higher resource usage).
    pub apm_config_compression_level: Option<i32>,
    /// @env `DD_APM_FEATURES`
    #[serde(deserialize_with = "deserialize_array_from_comma_separated_string")]
    pub apm_features: Vec<String>,
    /// @env `DD_APM_ADDITIONAL_ENDPOINTS`
    ///
    /// Additional endpoints to send traces to.
    /// <https://docs.datadoghq.com/agent/configuration/dual-shipping/?tab=helm#environment-variable-configuration-1>
    #[serde(deserialize_with = "deserialize_additional_endpoints")]
    pub apm_additional_endpoints: HashMap<String, Vec<String>>,
    /// @env `DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED`
    ///
    /// Enable the new AWS-resource naming logic in the tracer.
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_aws_service_representation_enabled: Option<bool>,
    //
    // Trace Propagation
    /// @env `DD_TRACE_PROPAGATION_STYLE`
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style: Vec<TracePropagationStyle>,
    /// @env `DD_TRACE_PROPAGATION_STYLE_EXTRACT`
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style_extract: Vec<TracePropagationStyle>,
    /// @env `DD_TRACE_PROPAGATION_EXTRACT_FIRST`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_propagation_extract_first: Option<bool>,
    /// @env `DD_TRACE_PROPAGATION_HTTP_BAGGAGE_ENABLED`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_propagation_http_baggage_enabled: Option<bool>,

    // OTLP
    //
    // - APM / Traces
    /// @env `DD_OTLP_CONFIG_TRACES_ENABLED`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_traces_enabled: Option<bool>,
    /// @env `DD_OTLP_CONFIG_TRACES_SPAN_NAME_AS_RESOURCE_NAME`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_traces_span_name_as_resource_name: Option<bool>,
    /// @env `DD_OTLP_CONFIG_TRACES_SPAN_NAME_REMAPPINGS`
    #[serde(deserialize_with = "deserialize_key_value_pairs")]
    pub otlp_config_traces_span_name_remappings: HashMap<String, String>,
    /// @env `DD_OTLP_CONFIG_IGNORE_MISSING_DATADOG_FIELDS`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_ignore_missing_datadog_fields: Option<bool>,
    //
    // - Receiver / HTTP
    /// @env `DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT`
    pub otlp_config_receiver_protocols_http_endpoint: Option<String>,
    // - Unsupported Configuration
    //
    // - Receiver / GRPC
    /// @env `DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_ENDPOINT`
    pub otlp_config_receiver_protocols_grpc_endpoint: Option<String>,
    /// @env `DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_TRANSPORT`
    pub otlp_config_receiver_protocols_grpc_transport: Option<String>,
    /// @env `DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_MAX_RECV_MSG_SIZE_MIB`
    pub otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib: Option<i32>,
    // - Metrics
    /// @env `DD_OTLP_CONFIG_METRICS_ENABLED`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_enabled: Option<bool>,
    /// @env `DD_OTLP_CONFIG_METRICS_RESOURCE_ATTRIBUTES_AS_TAGS`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_resource_attributes_as_tags: Option<bool>,
    /// @env `DD_OTLP_CONFIG_METRICS_INSTRUMENTATION_SCOPE_METADATA_AS_TAGS`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_instrumentation_scope_metadata_as_tags: Option<bool>,
    /// @env `DD_OTLP_CONFIG_METRICS_TAG_CARDINALITY`
    pub otlp_config_metrics_tag_cardinality: Option<String>,
    /// @env `DD_OTLP_CONFIG_METRICS_DELTA_TTL`
    pub otlp_config_metrics_delta_ttl: Option<i32>,
    /// @env `DD_OTLP_CONFIG_METRICS_HISTOGRAMS_MODE`
    pub otlp_config_metrics_histograms_mode: Option<String>,
    /// @env `DD_OTLP_CONFIG_METRICS_HISTOGRAMS_SEND_COUNT_SUM_METRICS`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_histograms_send_count_sum_metrics: Option<bool>,
    /// @env `DD_OTLP_CONFIG_METRICS_HISTOGRAMS_SEND_AGGREGATION_METRICS`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_histograms_send_aggregation_metrics: Option<bool>,
    pub otlp_config_metrics_sums_cumulative_monotonic_mode: Option<String>,
    /// @env `DD_OTLP_CONFIG_METRICS_SUMS_INITIAL_CUMULATIVE_MONOTONIC_VALUE`
    pub otlp_config_metrics_sums_initial_cumulativ_monotonic_value: Option<String>,
    /// @env `DD_OTLP_CONFIG_METRICS_SUMMARIES_MODE`
    pub otlp_config_metrics_summaries_mode: Option<String>,
    // - Traces
    /// @env `DD_OTLP_CONFIG_TRACES_PROBABILISTIC_SAMPLER_SAMPLING_PERCENTAGE`
    pub otlp_config_traces_probabilistic_sampler_sampling_percentage: Option<i32>,
    // - Logs
    /// @env `DD_OTLP_CONFIG_LOGS_ENABLED`
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_logs_enabled: Option<bool>,

    // AWS Lambda
    /// @env `DD_API_KEY_SECRET_ARN`
    ///
    /// The AWS ARN of the secret containing the Datadog API key.
    pub api_key_secret_arn: Option<String>,
    /// @env `DD_KMS_API_KEY`
    ///
    /// The AWS KMS API key to use for the Datadog Agent.
    pub kms_api_key: Option<String>,
    /// @env `DD_SERVERLESS_LOGS_ENABLED`
    ///
    /// Enable logs for AWS Lambda. Default is `true`.
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub serverless_logs_enabled: Option<bool>,
    /// @env `DD_SERVERLESS_FLUSH_STRATEGY`
    ///
    /// The flush strategy to use for AWS Lambda.
    pub serverless_flush_strategy: Option<FlushStrategy>,
    /// @env `DD_ENHANCED_METRICS`
    ///
    /// Enable enhanced metrics for AWS Lambda. Default is `true`.
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub enhanced_metrics: Option<bool>,
    /// @env `DD_LAMBDA_PROC_ENHANCED_METRICS`
    ///
    /// Enable Lambda process metrics for AWS Lambda. Default is `true`.
    ///
    /// This is for metrics like:
    /// - CPU usage
    /// - Network usage
    /// - File descriptor count
    /// - Thread count
    /// - Temp directory usage
    pub lambda_proc_enhanced_metrics: Option<bool>,
    /// @env `DD_CAPTURE_LAMBDA_PAYLOAD`
    ///
    /// Enable capture of the Lambda request and response payloads.
    /// Default is `false`.
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub capture_lambda_payload: Option<bool>,
    /// @env `DD_CAPTURE_LAMBDA_PAYLOAD_MAX_DEPTH`
    ///
    /// The maximum depth of the Lambda payload to capture.
    /// Default is `10`. Requires `capture_lambda_payload` to be `true`.
    pub capture_lambda_payload_max_depth: Option<u32>,
    /// @env `DD_SERVERLESS_APPSEC_ENABLED`
    ///
    /// Enable Application and API Protection (AAP), previously known as AppSec/ASM, for AWS Lambda.
    /// Default is `false`.
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub serverless_appsec_enabled: Option<bool>,
    /// @env `DD_EXTENSION_VERSION`
    ///
    /// Used to decide which version of the Datadog Lambda Extension to use.
    /// When set to `compatibility`, the extension will boot up in legacy mode.
    pub extension_version: Option<String>,
}

#[allow(clippy::too_many_lines)]
fn merge_config(config: &mut Config, env_config: &EnvConfig, start_time: Instant) {
    // Basic fields
    merge_string!(config, env_config, site);
    merge_string!(config, env_config, api_key);
    merge_option_to_value!(config, env_config, log_level);
    merge_option_to_value!(config, env_config, flush_timeout);

    // Unified Service Tagging
    merge_option!(config, env_config, env);
    merge_option!(config, env_config, service);
    merge_option!(config, env_config, version);
    merge_hashmap!(config, env_config, tags);

    // Proxy
    merge_option!(config, env_config, proxy_https);
    merge_vec!(config, env_config, proxy_no_proxy);
    merge_option!(config, env_config, http_protocol);

    // Endpoints
    merge_string!(config, env_config, dd_url);
    merge_string!(config, env_config, url);
    merge_hashmap!(config, env_config, additional_endpoints);

    // Logs
    merge_string!(config, env_config, logs_config_logs_dd_url);
    merge_option!(config, env_config, logs_config_processing_rules);
    merge_option_to_value!(config, env_config, logs_config_use_compression);
    merge_option_to_value!(config, env_config, logs_config_compression_level);
    merge_vec!(config, env_config, logs_config_additional_endpoints);

    // APM
    merge_hashmap!(config, env_config, service_mapping);
    merge_string!(config, env_config, apm_dd_url);
    merge_option!(config, env_config, apm_replace_tags);
    merge_option_to_value!(
        config,
        env_config,
        apm_config_obfuscation_http_remove_query_string
    );
    merge_option_to_value!(
        config,
        env_config,
        apm_config_obfuscation_http_remove_paths_with_digits
    );
    merge_option_to_value!(config, env_config, apm_config_compression_level);
    merge_vec!(config, env_config, apm_features);
    merge_hashmap!(config, env_config, apm_additional_endpoints);
    merge_option_to_value!(config, env_config, trace_aws_service_representation_enabled);

    // Trace Propagation
    merge_vec!(config, env_config, trace_propagation_style);
    merge_vec!(config, env_config, trace_propagation_style_extract);
    merge_option_to_value!(config, env_config, trace_propagation_extract_first);
    merge_option_to_value!(config, env_config, trace_propagation_http_baggage_enabled);

    // OTLP
    merge_option_to_value!(config, env_config, otlp_config_traces_enabled);
    merge_option_to_value!(
        config,
        env_config,
        otlp_config_traces_span_name_as_resource_name
    );
    merge_hashmap!(config, env_config, otlp_config_traces_span_name_remappings);
    merge_option_to_value!(
        config,
        env_config,
        otlp_config_ignore_missing_datadog_fields
    );
    merge_option!(
        config,
        env_config,
        otlp_config_receiver_protocols_http_endpoint
    );
    merge_option!(
        config,
        env_config,
        otlp_config_receiver_protocols_grpc_endpoint
    );
    merge_option!(
        config,
        env_config,
        otlp_config_receiver_protocols_grpc_transport
    );
    merge_option!(
        config,
        env_config,
        otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
    );
    merge_option_to_value!(config, env_config, otlp_config_metrics_enabled);
    merge_option_to_value!(
        config,
        env_config,
        otlp_config_metrics_resource_attributes_as_tags
    );
    merge_option_to_value!(
        config,
        env_config,
        otlp_config_metrics_instrumentation_scope_metadata_as_tags
    );
    merge_option!(config, env_config, otlp_config_metrics_tag_cardinality);
    merge_option!(config, env_config, otlp_config_metrics_delta_ttl);
    merge_option!(config, env_config, otlp_config_metrics_histograms_mode);
    merge_option_to_value!(
        config,
        env_config,
        otlp_config_metrics_histograms_send_count_sum_metrics
    );
    merge_option_to_value!(
        config,
        env_config,
        otlp_config_metrics_histograms_send_aggregation_metrics
    );
    merge_option!(
        config,
        env_config,
        otlp_config_metrics_sums_cumulative_monotonic_mode
    );
    merge_option!(
        config,
        env_config,
        otlp_config_metrics_sums_initial_cumulativ_monotonic_value
    );
    merge_option!(config, env_config, otlp_config_metrics_summaries_mode);
    merge_option!(
        config,
        env_config,
        otlp_config_traces_probabilistic_sampler_sampling_percentage
    );
    merge_option_to_value!(config, env_config, otlp_config_logs_enabled);

    // AWS Lambda
    merge_string!(config, env_config, api_key_secret_arn);
    merge_string!(config, env_config, kms_api_key);
    merge_option_to_value!(config, env_config, serverless_logs_enabled);
    merge_option_to_value!(config, env_config, serverless_flush_strategy);
    merge_option_to_value!(config, env_config, enhanced_metrics);
    merge_option_to_value!(config, env_config, lambda_proc_enhanced_metrics);
    merge_option_to_value!(config, env_config, capture_lambda_payload);
    merge_option_to_value!(config, env_config, capture_lambda_payload_max_depth);
    merge_option_to_value!(config, env_config, serverless_appsec_enabled);
    merge_option!(config, env_config, extension_version);
}

use std::time::Instant;

#[derive(Debug, PartialEq, Clone, Copy)]
#[allow(clippy::module_name_repetitions)]
pub struct EnvConfigSource;

impl ConfigSource for EnvConfigSource {
    fn load(&self, config: &mut Config, start_time: Instant) -> Result<(), ConfigError> {
        println!("  Entered EnvConfigSource::load(). {:?} ms", start_time.elapsed().as_millis().to_string());
        let figment = Figment::new()
            .merge(Env::prefixed("DATADOG_"))
            .merge(Env::prefixed("DD_"));

        println!("  In EnvConfigSource::load(), figment created: {:?} ms", start_time.elapsed().as_millis().to_string());

        match figment.extract::<EnvConfig>() {
            Ok(env_config) => {
                println!("In EnvConfigSource::load(), env_config created: {:?} ms", start_time.elapsed().as_millis().to_string());
                merge_config(config, &env_config, start_time);
                println!("In EnvConfigSource::load(), merge_config done: {:?} ms", start_time.elapsed().as_millis().to_string());
            }
            Err(e) => {
                return Err(ConfigError::ParseError(format!(
                    "Failed to parse config from environment variables: {e}, using default config.",
                )));
            }
        }

        println!("  Finished EnvConfigSource::load(). {:?} ms", start_time.elapsed().as_millis().to_string());

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config,
        flush_strategy::{FlushStrategy, PeriodicStrategy},
        log_level::LogLevel,
        processing_rule::{Kind, ProcessingRule},
        trace_propagation_style::TracePropagationStyle,
    };

    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_merge_config_overrides_with_environment_variables() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();

            // Set environment variables here
            jail.set_env("DD_SITE", "test-site");
            jail.set_env("DD_API_KEY", "test-api-key");
            jail.set_env("DD_LOG_LEVEL", "debug");
            jail.set_env("DD_FLUSH_TIMEOUT", "42");

            // Proxy
            jail.set_env("DD_PROXY_HTTPS", "https://proxy.example.com");
            jail.set_env("DD_PROXY_NO_PROXY", "localhost,127.0.0.1");
            jail.set_env("DD_HTTP_PROTOCOL", "http1");

            // Metrics
            jail.set_env("DD_DD_URL", "https://metrics.datadoghq.com");
            jail.set_env("DD_URL", "https://app.datadoghq.com");
            jail.set_env(
                "DD_ADDITIONAL_ENDPOINTS",
                "{\"https://app.datadoghq.com\": [\"apikey2\", \"apikey3\"], \"https://app.datadoghq.eu\": [\"apikey4\"]}",
            );

            // Unified Service Tagging
            jail.set_env("DD_ENV", "test-env");
            jail.set_env("DD_SERVICE", "test-service");
            jail.set_env("DD_VERSION", "1.0.0");
            jail.set_env("DD_TAGS", "team:test-team,project:test-project");

            // Logs
            jail.set_env("DD_LOGS_CONFIG_LOGS_DD_URL", "https://logs.datadoghq.com");
            jail.set_env(
                "DD_LOGS_CONFIG_PROCESSING_RULES",
                r#"[{"type":"exclude_at_match","name":"exclude","pattern":"exclude"}]"#,
            );
            jail.set_env("DD_LOGS_CONFIG_USE_COMPRESSION", "false");
            jail.set_env("DD_LOGS_CONFIG_COMPRESSION_LEVEL", "3");
            jail.set_env(
                "DD_LOGS_CONFIG_ADDITIONAL_ENDPOINTS",
                "[{\"api_key\": \"apikey2\", \"Host\": \"agent-http-intake.logs.datadoghq.com\", \"Port\": 443, \"is_reliable\": true}]",
            );

            // APM
            jail.set_env("DD_SERVICE_MAPPING", "old-service:new-service");
            jail.set_env("DD_APPSEC_ENABLED", "true");
            jail.set_env("DD_APM_DD_URL", "https://apm.datadoghq.com");
            jail.set_env(
                "DD_APM_REPLACE_TAGS",
                r#"[{"name":"test-tag","pattern":"test-pattern","repl":"replacement"}]"#,
            );
            jail.set_env("DD_APM_CONFIG_OBFUSCATION_HTTP_REMOVE_QUERY_STRING", "true");
            jail.set_env(
                "DD_APM_CONFIG_OBFUSCATION_HTTP_REMOVE_PATHS_WITH_DIGITS",
                "true",
            );
            jail.set_env("DD_APM_CONFIG_COMPRESSION_LEVEL", "3");
            jail.set_env(
                "DD_APM_FEATURES",
                "enable_otlp_compute_top_level_by_span_kind,enable_stats_by_span_kind",
            );
            jail.set_env("DD_APM_ADDITIONAL_ENDPOINTS", "{\"https://trace.agent.datadoghq.com\": [\"apikey2\", \"apikey3\"], \"https://trace.agent.datadoghq.eu\": [\"apikey4\"]}");

            // Trace Propagation
            jail.set_env("DD_TRACE_PROPAGATION_STYLE", "datadog");
            jail.set_env("DD_TRACE_PROPAGATION_STYLE_EXTRACT", "b3");
            jail.set_env("DD_TRACE_PROPAGATION_EXTRACT_FIRST", "true");
            jail.set_env("DD_TRACE_PROPAGATION_HTTP_BAGGAGE_ENABLED", "true");
            jail.set_env("DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED", "true");

            // OTLP
            jail.set_env("DD_OTLP_CONFIG_TRACES_ENABLED", "false");
            jail.set_env("DD_OTLP_CONFIG_TRACES_SPAN_NAME_AS_RESOURCE_NAME", "true");
            jail.set_env(
                "DD_OTLP_CONFIG_TRACES_SPAN_NAME_REMAPPINGS",
                "old-span:new-span",
            );
            jail.set_env("DD_OTLP_CONFIG_IGNORE_MISSING_DATADOG_FIELDS", "true");
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_HTTP_ENDPOINT",
                "http://localhost:4318",
            );
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_ENDPOINT",
                "http://localhost:4317",
            );
            jail.set_env("DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_TRANSPORT", "tcp");
            jail.set_env(
                "DD_OTLP_CONFIG_RECEIVER_PROTOCOLS_GRPC_MAX_RECV_MSG_SIZE_MIB",
                "4",
            );
            jail.set_env("DD_OTLP_CONFIG_METRICS_ENABLED", "true");
            jail.set_env("DD_OTLP_CONFIG_METRICS_RESOURCE_ATTRIBUTES_AS_TAGS", "true");
            jail.set_env(
                "DD_OTLP_CONFIG_METRICS_INSTRUMENTATION_SCOPE_METADATA_AS_TAGS",
                "true",
            );
            jail.set_env("DD_OTLP_CONFIG_METRICS_TAG_CARDINALITY", "low");
            jail.set_env("DD_OTLP_CONFIG_METRICS_DELTA_TTL", "3600");
            jail.set_env("DD_OTLP_CONFIG_METRICS_HISTOGRAMS_MODE", "counters");
            jail.set_env(
                "DD_OTLP_CONFIG_METRICS_HISTOGRAMS_SEND_COUNT_SUM_METRICS",
                "true",
            );
            jail.set_env(
                "DD_OTLP_CONFIG_METRICS_HISTOGRAMS_SEND_AGGREGATION_METRICS",
                "true",
            );
            jail.set_env(
                "DD_OTLP_CONFIG_METRICS_SUMS_CUMULATIVE_MONOTONIC_MODE",
                "to_delta",
            );
            jail.set_env(
                "DD_OTLP_CONFIG_METRICS_SUMS_INITIAL_CUMULATIV_MONOTONIC_VALUE",
                "auto",
            );
            jail.set_env("DD_OTLP_CONFIG_METRICS_SUMMARIES_MODE", "quantiles");
            jail.set_env(
                "DD_OTLP_CONFIG_TRACES_PROBABILISTIC_SAMPLER_SAMPLING_PERCENTAGE",
                "50",
            );
            jail.set_env("DD_OTLP_CONFIG_LOGS_ENABLED", "true");

            // AWS Lambda
            jail.set_env(
                "DD_API_KEY_SECRET_ARN",
                "arn:aws:secretsmanager:region:account:secret:datadog-api-key",
            );
            jail.set_env("DD_KMS_API_KEY", "test-kms-key");
            jail.set_env("DD_SERVERLESS_LOGS_ENABLED", "false");
            jail.set_env("DD_SERVERLESS_FLUSH_STRATEGY", "periodically,60000");
            jail.set_env("DD_ENHANCED_METRICS", "false");
            jail.set_env("DD_LAMBDA_PROC_ENHANCED_METRICS", "false");
            jail.set_env("DD_CAPTURE_LAMBDA_PAYLOAD", "true");
            jail.set_env("DD_CAPTURE_LAMBDA_PAYLOAD_MAX_DEPTH", "5");
            jail.set_env("DD_SERVERLESS_APPSEC_ENABLED", "true");
            jail.set_env("DD_EXTENSION_VERSION", "compatibility");

            let mut config = Config::default();
            let env_config_source = EnvConfigSource;
            env_config_source
                .load(&mut config)
                .expect("Failed to load config");

            let expected_config = Config {
                site: "test-site".to_string(),
                api_key: "test-api-key".to_string(),
                log_level: LogLevel::Debug,
                flush_timeout: 42,
                proxy_https: Some("https://proxy.example.com".to_string()),
                proxy_no_proxy: vec!["localhost".to_string(), "127.0.0.1".to_string()],
                http_protocol: Some("http1".to_string()),
                dd_url: "https://metrics.datadoghq.com".to_string(),
                url: "https://app.datadoghq.com".to_string(),
                additional_endpoints: HashMap::from([
                    (
                        "https://app.datadoghq.com".to_string(),
                        vec!["apikey2".to_string(), "apikey3".to_string()],
                    ),
                    (
                        "https://app.datadoghq.eu".to_string(),
                        vec!["apikey4".to_string()],
                    ),
                ]),
                env: Some("test-env".to_string()),
                service: Some("test-service".to_string()),
                version: Some("1.0.0".to_string()),
                tags: HashMap::from([
                    ("team".to_string(), "test-team".to_string()),
                    ("project".to_string(), "test-project".to_string()),
                ]),
                logs_config_logs_dd_url: "https://logs.datadoghq.com".to_string(),
                logs_config_processing_rules: Some(vec![ProcessingRule {
                    kind: Kind::ExcludeAtMatch,
                    name: "exclude".to_string(),
                    pattern: "exclude".to_string(),
                    replace_placeholder: None,
                }]),
                logs_config_use_compression: false,
                logs_config_compression_level: 3,
                logs_config_additional_endpoints: vec![LogsAdditionalEndpoint {
                    api_key: "apikey2".to_string(),
                    host: "agent-http-intake.logs.datadoghq.com".to_string(),
                    port: 443,
                    is_reliable: true,
                }],
                service_mapping: HashMap::from([(
                    "old-service".to_string(),
                    "new-service".to_string(),
                )]),
                apm_dd_url: "https://apm.datadoghq.com".to_string(),
                apm_replace_tags: Some(
                    datadog_trace_obfuscation::replacer::parse_rules_from_string(
                        r#"[{"name":"test-tag","pattern":"test-pattern","repl":"replacement"}]"#,
                    )
                    .expect("Failed to parse replace rules"),
                ),
                apm_config_obfuscation_http_remove_query_string: true,
                apm_config_obfuscation_http_remove_paths_with_digits: true,
                apm_config_compression_level: 3,
                apm_features: vec![
                    "enable_otlp_compute_top_level_by_span_kind".to_string(),
                    "enable_stats_by_span_kind".to_string(),
                ],
                apm_additional_endpoints: HashMap::from([
                    (
                        "https://trace.agent.datadoghq.com".to_string(),
                        vec!["apikey2".to_string(), "apikey3".to_string()],
                    ),
                    (
                        "https://trace.agent.datadoghq.eu".to_string(),
                        vec!["apikey4".to_string()],
                    ),
                ]),
                trace_propagation_style: vec![TracePropagationStyle::Datadog],
                trace_propagation_style_extract: vec![TracePropagationStyle::B3],
                trace_propagation_extract_first: true,
                trace_propagation_http_baggage_enabled: true,
                trace_aws_service_representation_enabled: true,
                otlp_config_traces_enabled: false,
                otlp_config_traces_span_name_as_resource_name: true,
                otlp_config_traces_span_name_remappings: HashMap::from([(
                    "old-span".to_string(),
                    "new-span".to_string(),
                )]),
                otlp_config_ignore_missing_datadog_fields: true,
                otlp_config_receiver_protocols_http_endpoint: Some(
                    "http://localhost:4318".to_string(),
                ),
                otlp_config_receiver_protocols_grpc_endpoint: Some(
                    "http://localhost:4317".to_string(),
                ),
                otlp_config_receiver_protocols_grpc_transport: Some("tcp".to_string()),
                otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib: Some(4),
                otlp_config_metrics_enabled: true,
                otlp_config_metrics_resource_attributes_as_tags: true,
                otlp_config_metrics_instrumentation_scope_metadata_as_tags: true,
                otlp_config_metrics_tag_cardinality: Some("low".to_string()),
                otlp_config_metrics_delta_ttl: Some(3600),
                otlp_config_metrics_histograms_mode: Some("counters".to_string()),
                otlp_config_metrics_histograms_send_count_sum_metrics: true,
                otlp_config_metrics_histograms_send_aggregation_metrics: true,
                otlp_config_metrics_sums_cumulative_monotonic_mode: Some("to_delta".to_string()),
                otlp_config_metrics_sums_initial_cumulativ_monotonic_value: Some(
                    "auto".to_string(),
                ),
                otlp_config_metrics_summaries_mode: Some("quantiles".to_string()),
                otlp_config_traces_probabilistic_sampler_sampling_percentage: Some(50),
                otlp_config_logs_enabled: true,
                api_key_secret_arn: "arn:aws:secretsmanager:region:account:secret:datadog-api-key"
                    .to_string(),
                kms_api_key: "test-kms-key".to_string(),
                serverless_logs_enabled: false,
                serverless_flush_strategy: FlushStrategy::Periodically(PeriodicStrategy {
                    interval: 60000,
                }),
                enhanced_metrics: false,
                lambda_proc_enhanced_metrics: false,
                capture_lambda_payload: true,
                capture_lambda_payload_max_depth: 5,
                serverless_appsec_enabled: true,
                extension_version: Some("compatibility".to_string()),
            };

            assert_eq!(config, expected_config);

            Ok(())
        });
    }
}
