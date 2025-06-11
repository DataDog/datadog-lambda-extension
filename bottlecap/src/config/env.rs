use crate::config::additional_endpoints::deserialize_additional_endpoints;
use serde::{Deserialize, Deserializer};
use figment::{providers::Env, Figment};
use std::collections::HashMap;

use datadog_trace_obfuscation::replacer::ReplaceRule;

use crate::config::{
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
    pub site: Option<String>,
    pub api_key: Option<String>,
    pub log_level: Option<LogLevel>,

    /// Flush timeout in seconds
    pub flush_timeout: Option<u64>, //TODO go agent adds jitter too

    // Proxy
    pub proxy_https: Option<String>,
    pub proxy_no_proxy: Vec<String>,
    pub dd_url: Option<String>,
    pub url: Option<String>,

    // Unified Service Tagging
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub env: Option<String>,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub service: Option<String>,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub version: Option<String>,
    #[serde(deserialize_with = "deserialize_key_value_pairs")]
    pub tags: HashMap<String, String>,

    // Logs
    pub logs_config_logs_dd_url: Option<String>,
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub logs_config_processing_rules: Option<Vec<ProcessingRule>>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub logs_config_use_compression: Option<bool>,
    pub logs_config_compression_level: Option<i32>,

    // APM
    //
    #[serde(deserialize_with = "deserialize_service_mapping")]
    pub service_mapping: HashMap<String, String>,
    //
    // Appsec
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub appsec_enabled: Option<bool>,
    //
    pub apm_config_apm_dd_url: Option<String>,
    #[serde(deserialize_with = "deserialize_apm_replace_rules")]
    pub apm_replace_tags: Option<Vec<ReplaceRule>>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub apm_config_obfuscation_http_remove_query_string: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub apm_config_obfuscation_http_remove_paths_with_digits: Option<bool>,
    pub apm_features: Vec<String>,
    //
    // Trace Propagation
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style: Vec<TracePropagationStyle>,
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style_extract: Vec<TracePropagationStyle>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_propagation_extract_first: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_propagation_http_baggage_enabled: Option<bool>,

    // OTLP
    //
    // - APM / Traces
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_traces_enabled: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_traces_span_name_as_resource_name: Option<bool>,
    pub otlp_config_traces_span_name_remappings: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_ignore_missing_datadog_fields: Option<bool>,
    //
    // - Receiver / HTTP
    pub otlp_config_receiver_protocols_http_endpoint: Option<String>,
    // - Unsupported Configuration
    //
    // - Receiver / GRPC
    pub otlp_config_receiver_protocols_grpc_endpoint: Option<String>,
    pub otlp_config_receiver_protocols_grpc_transport: Option<String>,
    pub otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib: Option<i32>,
    // - Metrics
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_enabled: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_resource_attributes_as_tags: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_instrumentation_scope_metadata_as_tags: Option<bool>,
    pub otlp_config_metrics_tag_cardinality: Option<String>,
    pub otlp_config_metrics_delta_ttl: Option<i32>,
    pub otlp_config_metrics_histograms_mode: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_histograms_send_count_sum_metrics: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_metrics_histograms_send_aggregation_metrics: Option<bool>,
    pub otlp_config_metrics_sums_cumulative_monotonic_mode: Option<String>,
    pub otlp_config_metrics_sums_initial_cumulativ_monotonic_value: Option<String>,
    pub otlp_config_metrics_summaries_mode: Option<String>,
    // - Traces
    pub otlp_config_traces_probabilistic_sampler_sampling_percentage: Option<i32>,
    // - Logs
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub otlp_config_logs_enabled: Option<bool>,

    // AWS Lambda
    pub api_key_secret_arn: Option<String>,
    pub kms_api_key: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub serverless_logs_enabled: Option<bool>,
    pub serverless_flush_strategy: Option<FlushStrategy>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub enhanced_metrics: Option<bool>,
    pub lambda_proc_enhanced_metrics: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub capture_lambda_payload: Option<bool>,
    pub capture_lambda_payload_max_depth: Option<u32>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub serverless_appsec_enabled: Option<bool>,
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
    if let Some(dd_url) = &env_config.dd_url {
        config.dd_url.clone_from(dd_url);
    }
    if let Some(url) = &env_config.url {
        config.url.clone_from(url);
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

    if let Some(apm_config_apm_dd_url) = &env_config.apm_config_apm_dd_url {
        config
            .apm_config_apm_dd_url
            .clone_from(apm_config_apm_dd_url);
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
