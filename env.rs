use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::vec;

use datadog_trace_obfuscation::replacer::ReplaceRule;
use serde_aux::field_attributes::deserialize_bool_from_anything;
use serde_json::Value;

use crate::config::{
    apm_replace_rule::deserialize_apm_replace_rules,
    flush_strategy::FlushStrategy,
    log_level::{deserialize_log_level, LogLevel},
    processing_rule::{deserialize_processing_rules, ProcessingRule},
    service_mapping::deserialize_service_mapping,
    trace_propagation_style::{deserialize_trace_propagation_style, TracePropagationStyle},
};

#[derive(Debug, PartialEq, Deserialize, Clone)]
#[serde(default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    pub site: String,
    pub api_key: String,
    pub api_key_secret_arn: String,
    pub kms_api_key: String,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub env: Option<String>,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub service: Option<String>,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub version: Option<String>,
    pub tags: Option<String>,
    #[serde(deserialize_with = "deserialize_log_level")]
    pub log_level: LogLevel,
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub logs_config_processing_rules: Option<Vec<ProcessingRule>>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub logs_config_use_compression: bool,
    pub logs_config_compression_level: i32,
    pub logs_config_logs_dd_url: String,
    pub serverless_flush_strategy: FlushStrategy,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub enhanced_metrics: bool,
    pub lambda_proc_enhanced_metrics: bool,
    /// Flush timeout in seconds
    pub flush_timeout: u64, //TODO go agent adds jitter too
    pub https_proxy: Option<String>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub capture_lambda_payload: bool,
    pub capture_lambda_payload_max_depth: u32,
    #[serde(deserialize_with = "deserialize_service_mapping")]
    pub service_mapping: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub serverless_logs_enabled: bool,
    // Trace Propagation
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style: Vec<TracePropagationStyle>,
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style_extract: Vec<TracePropagationStyle>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub trace_propagation_extract_first: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub trace_propagation_http_baggage_enabled: bool,
    // APM
    pub apm_config_apm_dd_url: String,
    #[serde(deserialize_with = "deserialize_apm_replace_rules")]
    pub apm_replace_tags: Option<Vec<ReplaceRule>>,
    #[serde(deserialize_with = "deserialize_apm_replace_rules")]
    pub apm_config_replace_tags: Option<Vec<ReplaceRule>>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub apm_config_obfuscation_http_remove_query_string: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub apm_config_obfuscation_http_remove_paths_with_digits: bool,
    pub apm_features: Vec<String>,
    pub apm_ignore_resources: Vec<String>,
    // Metrics overrides
    pub dd_url: String,
    pub url: String,
    // OTLP
    //
    // - Traces
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_traces_enabled: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_traces_span_name_as_resource_name: bool,
    pub otlp_config_traces_span_name_remappings: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_ignore_missing_datadog_fields: bool,
    // - Receiver / HTTP
    pub otlp_config_receiver_protocols_http_endpoint: Option<String>,
    //
    //
    // Fallback Config
    pub extension_version: Option<String>,
    // AppSec
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub serverless_appsec_enabled: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub appsec_enabled: bool,
    // OTLP
    //
    // - Receiver / GRPC
    pub otlp_config_receiver_protocols_grpc_endpoint: Option<String>,
    pub otlp_config_receiver_protocols_grpc_transport: Option<String>,
    pub otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib: Option<i32>,
    // - Metrics
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_metrics_enabled: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_metrics_resource_attributes_as_tags: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_metrics_instrumentation_scope_metadata_as_tags: bool,
    pub otlp_config_metrics_tag_cardinality: Option<String>,
    pub otlp_config_metrics_delta_ttl: Option<i32>,
    pub otlp_config_metrics_histograms_mode: Option<String>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_metrics_histograms_send_count_sum_metrics: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_metrics_histograms_send_aggregation_metrics: bool,
    pub otlp_config_metrics_sums_cumulative_monotonic_mode: Option<String>,
    pub otlp_config_metrics_sums_initial_cumulativ_monotonic_value: Option<String>,
    pub otlp_config_metrics_summaries_mode: Option<String>,
    // - Traces
    pub otlp_config_traces_probabilistic_sampler_sampling_percentage: Option<i32>,
    // - Logs
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub otlp_config_logs_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // General
            site: String::default(),
            api_key: String::default(),
            api_key_secret_arn: String::default(),
            kms_api_key: String::default(),
            serverless_flush_strategy: FlushStrategy::Default,
            flush_timeout: 5,
            // Unified Tagging
            env: None,
            service: None,
            version: None,
            tags: None,
            // Logs
            log_level: LogLevel::default(),
            logs_config_processing_rules: None,
            logs_config_use_compression: true,
            logs_config_compression_level: 6,
            logs_config_logs_dd_url: String::default(),
            // Metrics
            enhanced_metrics: true,
            lambda_proc_enhanced_metrics: true,
            https_proxy: None,
            capture_lambda_payload: false,
            capture_lambda_payload_max_depth: 10,
            service_mapping: HashMap::new(),
            serverless_logs_enabled: true,
            // Trace Propagation
            trace_propagation_style: vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
            ],
            trace_propagation_style_extract: vec![],
            trace_propagation_extract_first: false,
            trace_propagation_http_baggage_enabled: false,
            // APM
            apm_config_apm_dd_url: String::default(),
            apm_replace_tags: None,
            apm_config_replace_tags: None,
            apm_config_obfuscation_http_remove_query_string: false,
            apm_config_obfuscation_http_remove_paths_with_digits: false,
            apm_features: vec![],
            apm_ignore_resources: vec![],
            dd_url: String::default(),
            url: String::default(),
            // OTLP
            //
            // - Receiver
            otlp_config_receiver_protocols_http_endpoint: None,
            // - Traces
            otlp_config_traces_enabled: true,
            otlp_config_ignore_missing_datadog_fields: false,
            otlp_config_traces_span_name_as_resource_name: false,
            otlp_config_traces_span_name_remappings: HashMap::new(),
            //
            // Fallback Config (NOT SUPPORTED yet)
            extension_version: None,
            // AppSec
            serverless_appsec_enabled: false,
            appsec_enabled: false,
            // OTLP
            //
            // - Receiver
            otlp_config_receiver_protocols_grpc_endpoint: None,
            otlp_config_receiver_protocols_grpc_transport: None,
            otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib: None,
            // - Metrics
            otlp_config_metrics_enabled: false, // TODO(duncanista): Go Agent default is to true
            otlp_config_metrics_resource_attributes_as_tags: false,
            otlp_config_metrics_instrumentation_scope_metadata_as_tags: false,
            otlp_config_metrics_tag_cardinality: None,
            otlp_config_metrics_delta_ttl: None,
            otlp_config_metrics_histograms_mode: None,
            otlp_config_metrics_histograms_send_count_sum_metrics: false,
            otlp_config_metrics_histograms_send_aggregation_metrics: false,
            otlp_config_metrics_sums_cumulative_monotonic_mode: None,
            otlp_config_metrics_sums_initial_cumulativ_monotonic_value: None,
            otlp_config_metrics_summaries_mode: None,
            // - Traces
            otlp_config_traces_probabilistic_sampler_sampling_percentage: None,
            // - Logs
            otlp_config_logs_enabled: false,
        }
    }
}

fn deserialize_string_or_int<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(s))
            }
        }
        Value::Number(n) => Ok(Some(n.to_string())),
        _ => Err(serde::de::Error::custom("expected a string or an integer")),
    }
}
