use figment::{providers::Env, Figment};
use serde::Deserialize;
use std::collections::HashMap;

use datadog_trace_obfuscation::replacer::ReplaceRule;

use crate::config::{
    additional_endpoints::deserialize_additional_endpoints,
    apm_replace_rule::deserialize_apm_replace_rules,
    deserialize_key_value_pairs, deserialize_optional_bool_from_anything,
    deserialize_string_or_int,
    flush_strategy::FlushStrategy,
    log_level::LogLevel,
    processing_rule::{deserialize_processing_rules, ProcessingRule},
    service_mapping::deserialize_service_mapping,
    trace_propagation_style::{deserialize_trace_propagation_style, TracePropagationStyle},
    Config, ConfigError, ConfigSource,
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
    pub proxy_no_proxy: Vec<String>,
    /// @env `DD_HTTP_PROTOCOL`
    ///
    /// The HTTP protocol to use for the Datadog Agent.
    /// The transport type to use for sending logs. Possible values are "auto" or "http1".
    pub http_protocol: Option<String>,

    // Metrics
    /// @env `DD_DD_URL`
    ///
    /// The host of the Datadog intake server to send **metrics** to, only set this option
    /// if you need the Agent to send **metrics** to a custom URL, it overrides the site
    /// setting defined in "site". It does not affect APM, Logs, Remote Configuration,
    /// or Live Process intake which have their own "*_`dd_url`" settings.
    ///
    /// If `DD_DD_URL` and `DD_URL` are both set, `DD_DD_URL` is used in priority.
    pub dd_url: Option<String>,
    /// @env `DD_URL`
    pub url: Option<String>,

    /// @env `DD_ADDITIONAL_ENDPOINTS`
    ///
    /// Additional endpoints to send telemetry to.
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

    // APM
    //
    /// @env `DD_SERVICE_MAPPING`
    #[serde(deserialize_with = "deserialize_service_mapping")]
    pub service_mapping: HashMap<String, String>,
    //
    // Appsec
    /// @env `DD_APPSEC_ENABLED`
    ///
    /// Enable Application and API Protection (AAP), previously known as AppSec/ASM.
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub appsec_enabled: Option<bool>,
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
    /// @env `DD_APM_FEATURES`
    pub apm_features: Vec<String>,
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
    /// Enable enhanced metrics for AWS Lambda. Default is `true`.
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
fn merge_config(config: &mut Config, env_config: &EnvConfig) {
    if let Some(site) = &env_config.site {
        config.site.clone_from(site);
    }
    if let Some(api_key) = &env_config.api_key {
        config.api_key.clone_from(api_key);
    }
    if let Some(log_level) = &env_config.log_level {
        config.log_level.clone_from(log_level);
    }

    if let Some(flush_timeout) = &env_config.flush_timeout {
        config.flush_timeout.clone_from(flush_timeout);
    }

    // Unified Service Tagging
    if env_config.env.is_some() {
        config.env.clone_from(&env_config.env);
    }
    if env_config.service.is_some() {
        config.service.clone_from(&env_config.service);
    }
    if env_config.version.is_some() {
        config.version.clone_from(&env_config.version);
    }
    if !env_config.tags.is_empty() {
        config.tags.clone_from(&env_config.tags);
    }

    // Proxy
    if env_config.proxy_https.is_some() {
        config.proxy_https.clone_from(&env_config.proxy_https);
    }
    if !env_config.proxy_no_proxy.is_empty() {
        config.proxy_no_proxy.clone_from(&env_config.proxy_no_proxy);
    }
    if env_config.http_protocol.is_some() {
        config.http_protocol.clone_from(&env_config.http_protocol);
    }

    // Endpoints
    if let Some(dd_url) = &env_config.dd_url {
        config.dd_url.clone_from(dd_url);
    }
    if let Some(url) = &env_config.url {
        config.url.clone_from(url);
    }
    if !env_config.additional_endpoints.is_empty() {
        config
            .additional_endpoints
            .clone_from(&env_config.additional_endpoints);
    }

    // Logs
    if let Some(logs_config_logs_dd_url) = &env_config.logs_config_logs_dd_url {
        config
            .logs_config_logs_dd_url
            .clone_from(logs_config_logs_dd_url);
    }

    if env_config.logs_config_processing_rules.is_some() {
        config
            .logs_config_processing_rules
            .clone_from(&env_config.logs_config_processing_rules);
    }
    if let Some(logs_config_use_compression) = &env_config.logs_config_use_compression {
        config
            .logs_config_use_compression
            .clone_from(logs_config_use_compression);
    }

    if let Some(logs_config_compression_level) = &env_config.logs_config_compression_level {
        config
            .logs_config_compression_level
            .clone_from(logs_config_compression_level);
    }

    // APM
    if !env_config.service_mapping.is_empty() {
        config
            .service_mapping
            .clone_from(&env_config.service_mapping);
    }

    if let Some(appsec_enabled) = &env_config.appsec_enabled {
        config.appsec_enabled.clone_from(appsec_enabled);
    }

    if let Some(apm_dd_url) = &env_config.apm_dd_url {
        config.apm_dd_url.clone_from(apm_dd_url);
    }

    if env_config.apm_replace_tags.is_some() {
        config
            .apm_replace_tags
            .clone_from(&env_config.apm_replace_tags);
    }

    if let Some(apm_config_obfuscation_http_remove_query_string) =
        &env_config.apm_config_obfuscation_http_remove_query_string
    {
        config
            .apm_config_obfuscation_http_remove_query_string
            .clone_from(apm_config_obfuscation_http_remove_query_string);
    }

    if let Some(apm_config_obfuscation_http_remove_paths_with_digits) =
        &env_config.apm_config_obfuscation_http_remove_paths_with_digits
    {
        config
            .apm_config_obfuscation_http_remove_paths_with_digits
            .clone_from(apm_config_obfuscation_http_remove_paths_with_digits);
    }

    if !env_config.apm_features.is_empty() {
        config.apm_features.clone_from(&env_config.apm_features);
    }

    if !env_config.trace_propagation_style.is_empty() {
        config
            .trace_propagation_style
            .clone_from(&env_config.trace_propagation_style);
    }

    if !env_config.trace_propagation_style_extract.is_empty() {
        config
            .trace_propagation_style_extract
            .clone_from(&env_config.trace_propagation_style_extract);
    }

    if let Some(trace_propagation_extract_first) = &env_config.trace_propagation_extract_first {
        config
            .trace_propagation_extract_first
            .clone_from(trace_propagation_extract_first);
    }

    if let Some(trace_propagation_http_baggage_enabled) =
        &env_config.trace_propagation_http_baggage_enabled
    {
        config
            .trace_propagation_http_baggage_enabled
            .clone_from(trace_propagation_http_baggage_enabled);
    }

    // OTLP
    if let Some(otlp_config_traces_enabled) = &env_config.otlp_config_traces_enabled {
        config
            .otlp_config_traces_enabled
            .clone_from(otlp_config_traces_enabled);
    }

    if let Some(otlp_config_traces_span_name_as_resource_name) =
        &env_config.otlp_config_traces_span_name_as_resource_name
    {
        config
            .otlp_config_traces_span_name_as_resource_name
            .clone_from(otlp_config_traces_span_name_as_resource_name);
    }

    if !env_config
        .otlp_config_traces_span_name_remappings
        .is_empty()
    {
        config
            .otlp_config_traces_span_name_remappings
            .clone_from(&env_config.otlp_config_traces_span_name_remappings);
    }

    if let Some(otlp_config_ignore_missing_datadog_fields) =
        &env_config.otlp_config_ignore_missing_datadog_fields
    {
        config
            .otlp_config_ignore_missing_datadog_fields
            .clone_from(otlp_config_ignore_missing_datadog_fields);
    }

    if env_config
        .otlp_config_receiver_protocols_http_endpoint
        .is_some()
    {
        config
            .otlp_config_receiver_protocols_http_endpoint
            .clone_from(&env_config.otlp_config_receiver_protocols_http_endpoint);
    }

    if env_config
        .otlp_config_receiver_protocols_grpc_endpoint
        .is_some()
    {
        config
            .otlp_config_receiver_protocols_grpc_endpoint
            .clone_from(&env_config.otlp_config_receiver_protocols_grpc_endpoint);
    }

    if env_config
        .otlp_config_receiver_protocols_grpc_transport
        .is_some()
    {
        config
            .otlp_config_receiver_protocols_grpc_transport
            .clone_from(&env_config.otlp_config_receiver_protocols_grpc_transport);
    }

    if env_config
        .otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
        .is_some()
    {
        config
            .otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
            .clone_from(&env_config.otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib);
    }

    if let Some(otlp_config_metrics_enabled) = &env_config.otlp_config_metrics_enabled {
        config
            .otlp_config_metrics_enabled
            .clone_from(otlp_config_metrics_enabled);
    }

    if let Some(otlp_config_metrics_resource_attributes_as_tags) =
        &env_config.otlp_config_metrics_resource_attributes_as_tags
    {
        config
            .otlp_config_metrics_resource_attributes_as_tags
            .clone_from(otlp_config_metrics_resource_attributes_as_tags);
    }

    if let Some(otlp_config_metrics_instrumentation_scope_metadata_as_tags) =
        &env_config.otlp_config_metrics_instrumentation_scope_metadata_as_tags
    {
        config
            .otlp_config_metrics_instrumentation_scope_metadata_as_tags
            .clone_from(otlp_config_metrics_instrumentation_scope_metadata_as_tags);
    }

    if env_config.otlp_config_metrics_tag_cardinality.is_some() {
        config
            .otlp_config_metrics_tag_cardinality
            .clone_from(&env_config.otlp_config_metrics_tag_cardinality);
    }

    if env_config.otlp_config_metrics_delta_ttl.is_some() {
        config
            .otlp_config_metrics_delta_ttl
            .clone_from(&env_config.otlp_config_metrics_delta_ttl);
    }

    if env_config.otlp_config_metrics_histograms_mode.is_some() {
        config
            .otlp_config_metrics_histograms_mode
            .clone_from(&env_config.otlp_config_metrics_histograms_mode);
    }

    if let Some(otlp_config_metrics_histograms_send_count_sum_metrics) =
        &env_config.otlp_config_metrics_histograms_send_count_sum_metrics
    {
        config
            .otlp_config_metrics_histograms_send_count_sum_metrics
            .clone_from(otlp_config_metrics_histograms_send_count_sum_metrics);
    }

    if let Some(otlp_config_metrics_histograms_send_aggregation_metrics) =
        &env_config.otlp_config_metrics_histograms_send_aggregation_metrics
    {
        config
            .otlp_config_metrics_histograms_send_aggregation_metrics
            .clone_from(otlp_config_metrics_histograms_send_aggregation_metrics);
    }

    if env_config
        .otlp_config_metrics_sums_cumulative_monotonic_mode
        .is_some()
    {
        config
            .otlp_config_metrics_sums_cumulative_monotonic_mode
            .clone_from(&env_config.otlp_config_metrics_sums_cumulative_monotonic_mode);
    }

    if env_config
        .otlp_config_metrics_sums_initial_cumulativ_monotonic_value
        .is_some()
    {
        config
            .otlp_config_metrics_sums_initial_cumulativ_monotonic_value
            .clone_from(&env_config.otlp_config_metrics_sums_initial_cumulativ_monotonic_value);
    }

    if env_config.otlp_config_metrics_summaries_mode.is_some() {
        config
            .otlp_config_metrics_summaries_mode
            .clone_from(&env_config.otlp_config_metrics_summaries_mode);
    }

    if env_config
        .otlp_config_traces_probabilistic_sampler_sampling_percentage
        .is_some()
    {
        config
            .otlp_config_traces_probabilistic_sampler_sampling_percentage
            .clone_from(&env_config.otlp_config_traces_probabilistic_sampler_sampling_percentage);
    }

    if let Some(otlp_config_logs_enabled) = &env_config.otlp_config_logs_enabled {
        config
            .otlp_config_logs_enabled
            .clone_from(otlp_config_logs_enabled);
    }

    // AWS Lambda
    if let Some(api_key_secret_arn) = &env_config.api_key_secret_arn {
        config.api_key_secret_arn.clone_from(api_key_secret_arn);
    }
    if let Some(kms_api_key) = &env_config.kms_api_key {
        config.kms_api_key.clone_from(kms_api_key);
    }
    if let Some(serverless_logs_enabled) = &env_config.serverless_logs_enabled {
        config
            .serverless_logs_enabled
            .clone_from(serverless_logs_enabled);
    }
    if let Some(serverless_flush_strategy) = &env_config.serverless_flush_strategy {
        config
            .serverless_flush_strategy
            .clone_from(serverless_flush_strategy);
    }
    if let Some(enhanced_metrics) = &env_config.enhanced_metrics {
        config.enhanced_metrics.clone_from(enhanced_metrics);
    }
    if let Some(lambda_proc_enhanced_metrics) = &env_config.lambda_proc_enhanced_metrics {
        config
            .lambda_proc_enhanced_metrics
            .clone_from(lambda_proc_enhanced_metrics);
    }
    if let Some(capture_lambda_payload) = &env_config.capture_lambda_payload {
        config
            .capture_lambda_payload
            .clone_from(capture_lambda_payload);
    }
    if let Some(capture_lambda_payload_max_depth) = &env_config.capture_lambda_payload_max_depth {
        config
            .capture_lambda_payload_max_depth
            .clone_from(capture_lambda_payload_max_depth);
    }
    if let Some(serverless_appsec_enabled) = &env_config.serverless_appsec_enabled {
        config
            .serverless_appsec_enabled
            .clone_from(serverless_appsec_enabled);
    }

    if env_config.extension_version.is_some() {
        config
            .extension_version
            .clone_from(&env_config.extension_version);
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
#[allow(clippy::module_name_repetitions)]
pub struct EnvConfigSource;

impl ConfigSource for EnvConfigSource {
    fn load(&self, config: &mut Config) -> Result<(), ConfigError> {
        let figment = Figment::new()
            .merge(Env::prefixed("DATADOG_"))
            .merge(Env::prefixed("DD_"));

        match figment.extract::<EnvConfig>() {
            Ok(env_config) => merge_config(config, &env_config),
            Err(e) => {
                return Err(ConfigError::ParseError(format!(
                    "Failed to parse config from environment variables: {e}, using default config.",
                )));
            }
        }

        Ok(())
    }
}
