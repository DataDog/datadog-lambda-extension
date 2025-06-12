use std::{collections::HashMap, path::PathBuf};

use crate::config::{
    additional_endpoints::deserialize_additional_endpoints,
    deserialize_apm_replace_rules, deserialize_key_value_pairs,
    deserialize_optional_bool_from_anything, deserialize_processing_rules,
    deserialize_string_or_int,
    flush_strategy::FlushStrategy,
    log_level::LogLevel,
    service_mapping::deserialize_service_mapping,
    trace_propagation_style::{deserialize_trace_propagation_style, TracePropagationStyle},
    Config, ConfigError, ConfigSource, ProcessingRule,
};
use datadog_trace_obfuscation::replacer::ReplaceRule;
use figment::{
    providers::{Format, Yaml},
    Figment,
};
use serde::Deserialize;

/// `YamlConfig` is a struct that represents some of the fields in the `datadog.yaml` file.
///
/// It is used to deserialize the `datadog.yaml` file into a struct that can be merged
/// with the `Config` struct.
#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct YamlConfig {
    pub site: Option<String>,
    pub api_key: Option<String>,
    pub log_level: Option<LogLevel>,

    pub flush_timeout: Option<u64>,

    // Proxy
    pub proxy: ProxyConfig,
    pub dd_url: Option<String>,
    pub http_protocol: Option<String>,

    // Endpoints
    #[serde(deserialize_with = "deserialize_additional_endpoints")]
    /// Field used for Dual Shipping for Metrics
    pub additional_endpoints: HashMap<String, Vec<String>>,

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
    pub logs_config: LogsConfig,

    // APM
    pub apm_config: ApmConfig,
    #[serde(deserialize_with = "deserialize_service_mapping")]
    pub service_mapping: HashMap<String, String>,
    //
    // Appsec
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub appsec_enabled: Option<bool>,
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
    pub otlp_config: Option<OtlpConfig>,

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

/// Proxy Config
///

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct ProxyConfig {
    pub https: Option<String>,
    pub no_proxy: Option<Vec<String>>,
}

/// Logs Config
///

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct LogsConfig {
    pub logs_dd_url: Option<String>,
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub processing_rules: Option<Vec<ProcessingRule>>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub use_compression: Option<bool>,
    pub compression_level: Option<i32>,
}

/// APM Config
///

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct ApmConfig {
    pub apm_dd_url: Option<String>,
    #[serde(deserialize_with = "deserialize_apm_replace_rules")]
    pub replace_tags: Option<Vec<ReplaceRule>>,
    pub obfuscation: Option<ApmObfuscation>,
    pub features: Vec<String>,
}

impl ApmConfig {
    #[must_use]
    pub fn obfuscation_http_remove_query_string(&self) -> Option<bool> {
        self.obfuscation
            .as_ref()
            .and_then(|obfuscation| obfuscation.http.remove_query_string)
    }

    #[must_use]
    pub fn obfuscation_http_remove_paths_with_digits(&self) -> Option<bool> {
        self.obfuscation
            .as_ref()
            .and_then(|obfuscation| obfuscation.http.remove_paths_with_digits)
    }
}

#[derive(Debug, PartialEq, Deserialize, Clone, Copy, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct ApmObfuscation {
    pub http: ApmHttpObfuscation,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Copy, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct ApmHttpObfuscation {
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub remove_query_string: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub remove_paths_with_digits: Option<bool>,
}

/// OTLP Config
///

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpConfig {
    pub receiver: Option<OtlpReceiverConfig>,
    pub traces: Option<OtlpTracesConfig>,

    // NOT SUPPORTED
    pub metrics: Option<OtlpMetricsConfig>,
    pub logs: Option<OtlpLogsConfig>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverConfig {
    pub protocols: Option<OtlpReceiverProtocolsConfig>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverProtocolsConfig {
    pub http: Option<OtlpReceiverHttpConfig>,

    // NOT SUPPORTED
    pub grpc: Option<OtlpReceiverGrpcConfig>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverHttpConfig {
    pub endpoint: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverGrpcConfig {
    pub endpoint: Option<String>,
    pub transport: Option<String>,
    pub max_recv_msg_size_mib: Option<i32>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpTracesConfig {
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub enabled: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub span_name_as_resource_name: Option<bool>,
    pub span_name_remappings: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub ignore_missing_datadog_fields: Option<bool>,

    // NOT SUPORTED
    pub probabilistic_sampler: Option<OtlpTracesProbabilisticSampler>,
}

#[derive(Debug, PartialEq, Clone, Deserialize, Default, Copy)]
pub struct OtlpTracesProbabilisticSampler {
    pub sampling_percentage: Option<i32>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
pub struct OtlpMetricsConfig {
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub enabled: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub resource_attributes_as_tags: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub instrumentation_scope_metadata_as_tags: Option<bool>,
    pub tag_cardinality: Option<String>,
    pub delta_ttl: Option<i32>,
    pub histograms: Option<OtlpMetricsHistograms>,
    pub sums: Option<OtlpMetricsSums>,
    pub summaries: Option<OtlpMetricsSummaries>,
}

#[derive(Debug, PartialEq, Clone, Deserialize, Default)]
#[serde(default)]
pub struct OtlpMetricsHistograms {
    pub mode: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub send_count_sum_metrics: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub send_aggregation_metrics: Option<bool>,
}

#[derive(Debug, PartialEq, Clone, Deserialize, Default)]
#[serde(default)]
pub struct OtlpMetricsSums {
    pub cumulative_monotonic_mode: Option<String>,
    pub initial_cumulative_monotonic_value: Option<String>,
}

#[derive(Debug, PartialEq, Clone, Deserialize, Default)]
#[serde(default)]
pub struct OtlpMetricsSummaries {
    pub mode: Option<String>,
}

#[derive(Debug, PartialEq, Clone, Deserialize, Default, Copy)]
#[serde(default)]
pub struct OtlpLogsConfig {
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub enabled: Option<bool>,
}

impl OtlpConfig {
    #[must_use]
    pub fn receiver_protocols_http_endpoint(&self) -> Option<String> {
        self.receiver.as_ref().and_then(|receiver| {
            receiver.protocols.as_ref().and_then(|protocols| {
                protocols
                    .http
                    .as_ref()
                    .and_then(|http| http.endpoint.clone())
            })
        })
    }

    #[must_use]
    pub fn receiver_protocols_grpc(&self) -> Option<&OtlpReceiverGrpcConfig> {
        self.receiver.as_ref().and_then(|receiver| {
            receiver
                .protocols
                .as_ref()
                .and_then(|protocols| protocols.grpc.as_ref())
        })
    }

    #[must_use]
    pub fn traces_enabled(&self) -> Option<bool> {
        self.traces.as_ref().and_then(|traces| traces.enabled)
    }

    #[must_use]
    pub fn traces_ignore_missing_datadog_fields(&self) -> Option<bool> {
        self.traces
            .as_ref()
            .and_then(|traces| traces.ignore_missing_datadog_fields)
    }

    #[must_use]
    pub fn traces_span_name_as_resource_name(&self) -> Option<bool> {
        self.traces
            .as_ref()
            .and_then(|traces| traces.span_name_as_resource_name)
    }

    #[must_use]
    pub fn traces_span_name_remappings(&self) -> HashMap<String, String> {
        self.traces
            .as_ref()
            .map(|traces| traces.span_name_remappings.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn traces_probabilistic_sampler(&self) -> Option<&OtlpTracesProbabilisticSampler> {
        self.traces
            .as_ref()
            .and_then(|traces| traces.probabilistic_sampler.as_ref())
    }

    #[must_use]
    pub fn logs(&self) -> Option<&OtlpLogsConfig> {
        self.logs.as_ref()
    }
}

#[allow(clippy::too_many_lines)]
fn merge_config(config: &mut Config, yaml_config: &YamlConfig) {
    if let Some(site) = &yaml_config.site {
        config.site.clone_from(site);
    }
    if let Some(api_key) = &yaml_config.api_key {
        config.api_key.clone_from(api_key);
    }
    if let Some(log_level) = &yaml_config.log_level {
        config.log_level.clone_from(log_level);
    }

    if let Some(flush_timeout) = &yaml_config.flush_timeout {
        config.flush_timeout.clone_from(flush_timeout);
    }

    // Unified Service Tagging
    if yaml_config.env.is_some() {
        config.env.clone_from(&yaml_config.env);
    }
    if yaml_config.service.is_some() {
        config.service.clone_from(&yaml_config.service);
    }
    if yaml_config.version.is_some() {
        config.version.clone_from(&yaml_config.version);
    }
    if !yaml_config.tags.is_empty() {
        config.tags.clone_from(&yaml_config.tags);
    }

    // Proxy
    if yaml_config.proxy.https.is_some() {
        config.proxy_https.clone_from(&yaml_config.proxy.https);
    }
    if let Some(no_proxy) = &yaml_config.proxy.no_proxy {
        config.proxy_no_proxy.clone_from(no_proxy);
    }
    if yaml_config.http_protocol.is_some() {
        config.http_protocol.clone_from(&yaml_config.http_protocol);
    }

    // Endpoints
    if !yaml_config.additional_endpoints.is_empty() {
        config
            .additional_endpoints
            .clone_from(&yaml_config.additional_endpoints);
    }

    // This is the equivalent of `DD_DD_URL` in environment variables.
    // `DD_DD_URL` takes priority over `DD_URL` in environment variables.
    if let Some(dd_url) = &yaml_config.dd_url {
        config.dd_url.clone_from(dd_url);
    }

    // Logs
    if let Some(logs_dd_url) = &yaml_config.logs_config.logs_dd_url {
        config.logs_config_logs_dd_url.clone_from(logs_dd_url);
    }

    if yaml_config.logs_config.processing_rules.is_some() {
        config
            .logs_config_processing_rules
            .clone_from(&yaml_config.logs_config.processing_rules);
    }

    if let Some(use_compression) = &yaml_config.logs_config.use_compression {
        config
            .logs_config_use_compression
            .clone_from(use_compression);
    }

    if let Some(compression_level) = &yaml_config.logs_config.compression_level {
        config
            .logs_config_compression_level
            .clone_from(compression_level);
    }

    // APM
    if !yaml_config.service_mapping.is_empty() {
        config
            .service_mapping
            .clone_from(&yaml_config.service_mapping);
    }

    if let Some(appsec_enabled) = &yaml_config.appsec_enabled {
        config.appsec_enabled.clone_from(appsec_enabled);
    }

    if let Some(apm_dd_url) = &yaml_config.apm_config.apm_dd_url {
        config.apm_dd_url.clone_from(apm_dd_url);
    }

    if yaml_config.apm_config.replace_tags.is_some() {
        config
            .apm_replace_tags
            .clone_from(&yaml_config.apm_config.replace_tags);
    }

    if let Some(remove_query_string) = yaml_config
        .apm_config
        .obfuscation_http_remove_query_string()
    {
        config
            .apm_config_obfuscation_http_remove_query_string
            .clone_from(&remove_query_string);
    }

    if let Some(remove_paths_with_digits) = yaml_config
        .apm_config
        .obfuscation_http_remove_paths_with_digits()
    {
        config
            .apm_config_obfuscation_http_remove_paths_with_digits
            .clone_from(&remove_paths_with_digits);
    }

    if !yaml_config.apm_config.features.is_empty() {
        config
            .apm_features
            .clone_from(&yaml_config.apm_config.features);
    }

    if !yaml_config.trace_propagation_style.is_empty() {
        config
            .trace_propagation_style
            .clone_from(&yaml_config.trace_propagation_style);
    }

    if !yaml_config.trace_propagation_style_extract.is_empty() {
        config
            .trace_propagation_style_extract
            .clone_from(&yaml_config.trace_propagation_style_extract);
    }

    if let Some(trace_propagation_extract_first) = yaml_config.trace_propagation_extract_first {
        config
            .trace_propagation_extract_first
            .clone_from(&trace_propagation_extract_first);
    }

    if let Some(trace_propagation_http_baggage_enabled) =
        yaml_config.trace_propagation_http_baggage_enabled
    {
        config
            .trace_propagation_http_baggage_enabled
            .clone_from(&trace_propagation_http_baggage_enabled);
    }

    // OTLP
    if let Some(otlp_config) = &yaml_config.otlp_config {
        // Traces
        if let Some(traces_enabled) = otlp_config.traces_enabled() {
            config
                .otlp_config_traces_enabled
                .clone_from(&traces_enabled);
        }
        if let Some(traces_span_name_as_resource_name) =
            otlp_config.traces_span_name_as_resource_name()
        {
            config
                .otlp_config_traces_span_name_as_resource_name
                .clone_from(&traces_span_name_as_resource_name);
        }

        let traces_span_name_remappings = otlp_config.traces_span_name_remappings();
        if !traces_span_name_remappings.is_empty() {
            config
                .otlp_config_traces_span_name_remappings
                .clone_from(&traces_span_name_remappings);
        }
        if let Some(traces_ignore_missing_datadog_fields) =
            otlp_config.traces_ignore_missing_datadog_fields()
        {
            config
                .otlp_config_ignore_missing_datadog_fields
                .clone_from(&traces_ignore_missing_datadog_fields);
        }

        if let Some(probabilistic_sampler) = otlp_config.traces_probabilistic_sampler() {
            if probabilistic_sampler.sampling_percentage.is_some() {
                config
                    .otlp_config_traces_probabilistic_sampler_sampling_percentage
                    .clone_from(&probabilistic_sampler.sampling_percentage);
            }
        }

        // Receiver
        let receiver_protocols_http_endpoint = otlp_config.receiver_protocols_http_endpoint();
        if receiver_protocols_http_endpoint.is_some() {
            config
                .otlp_config_receiver_protocols_http_endpoint
                .clone_from(&receiver_protocols_http_endpoint);
        }

        if let Some(receiver_protocols_grpc) = otlp_config.receiver_protocols_grpc() {
            if receiver_protocols_grpc.endpoint.is_some() {
                config
                    .otlp_config_receiver_protocols_grpc_endpoint
                    .clone_from(&receiver_protocols_grpc.endpoint);
            }
            if receiver_protocols_grpc.transport.is_some() {
                config
                    .otlp_config_receiver_protocols_grpc_transport
                    .clone_from(&receiver_protocols_grpc.transport);
            }
            if receiver_protocols_grpc.max_recv_msg_size_mib.is_some() {
                config
                    .otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
                    .clone_from(&receiver_protocols_grpc.max_recv_msg_size_mib);
            }
        }

        // Metrics
        if let Some(metrics) = &otlp_config.metrics {
            if let Some(enabled) = metrics.enabled {
                config.otlp_config_metrics_enabled.clone_from(&enabled);
            }
            if let Some(resource_attributes_as_tags) = metrics.resource_attributes_as_tags {
                config
                    .otlp_config_metrics_resource_attributes_as_tags
                    .clone_from(&resource_attributes_as_tags);
            }
            if let Some(instrumentation_scope_metadata_as_tags) =
                metrics.instrumentation_scope_metadata_as_tags
            {
                config
                    .otlp_config_metrics_instrumentation_scope_metadata_as_tags
                    .clone_from(&instrumentation_scope_metadata_as_tags);
            }
            if metrics.tag_cardinality.is_some() {
                config
                    .otlp_config_metrics_tag_cardinality
                    .clone_from(&metrics.tag_cardinality);
            }
            if metrics.delta_ttl.is_some() {
                config
                    .otlp_config_metrics_delta_ttl
                    .clone_from(&metrics.delta_ttl);
            }
            if let Some(histograms) = &metrics.histograms {
                if histograms.mode.is_some() {
                    config
                        .otlp_config_metrics_histograms_mode
                        .clone_from(&histograms.mode);
                }
                if let Some(send_count_sum_metrics) = histograms.send_count_sum_metrics {
                    config
                        .otlp_config_metrics_histograms_send_count_sum_metrics
                        .clone_from(&send_count_sum_metrics);
                }
                if let Some(send_aggregation_metrics) = histograms.send_aggregation_metrics {
                    config
                        .otlp_config_metrics_histograms_send_aggregation_metrics
                        .clone_from(&send_aggregation_metrics);
                }
            }
            if let Some(sums) = &metrics.sums {
                if sums.cumulative_monotonic_mode.is_some() {
                    config
                        .otlp_config_metrics_sums_cumulative_monotonic_mode
                        .clone_from(&sums.cumulative_monotonic_mode);
                }
                if sums.initial_cumulative_monotonic_value.is_some() {
                    config
                        .otlp_config_metrics_sums_initial_cumulativ_monotonic_value
                        .clone_from(&sums.initial_cumulative_monotonic_value);
                }
            }
            if let Some(summaries) = &metrics.summaries {
                if summaries.mode.is_some() {
                    config
                        .otlp_config_metrics_summaries_mode
                        .clone_from(&summaries.mode);
                }
            }
        }

        // Logs
        if let Some(logs) = &otlp_config.logs {
            if let Some(enabled) = logs.enabled {
                config.otlp_config_logs_enabled.clone_from(&enabled);
            }
        }
    }

    // AWS Lambda
    if let Some(api_key_secret_arn) = &yaml_config.api_key_secret_arn {
        config.api_key_secret_arn.clone_from(api_key_secret_arn);
    }
    if let Some(kms_api_key) = &yaml_config.kms_api_key {
        config.kms_api_key.clone_from(kms_api_key);
    }
    if let Some(serverless_logs_enabled) = &yaml_config.serverless_logs_enabled {
        config
            .serverless_logs_enabled
            .clone_from(serverless_logs_enabled);
    }
    if let Some(serverless_flush_strategy) = &yaml_config.serverless_flush_strategy {
        config
            .serverless_flush_strategy
            .clone_from(serverless_flush_strategy);
    }
    if let Some(enhanced_metrics) = &yaml_config.enhanced_metrics {
        config.enhanced_metrics.clone_from(enhanced_metrics);
    }
    if let Some(lambda_proc_enhanced_metrics) = &yaml_config.lambda_proc_enhanced_metrics {
        config
            .lambda_proc_enhanced_metrics
            .clone_from(lambda_proc_enhanced_metrics);
    }
    if let Some(capture_lambda_payload) = &yaml_config.capture_lambda_payload {
        config
            .capture_lambda_payload
            .clone_from(capture_lambda_payload);
    }
    if let Some(capture_lambda_payload_max_depth) = &yaml_config.capture_lambda_payload_max_depth {
        config
            .capture_lambda_payload_max_depth
            .clone_from(capture_lambda_payload_max_depth);
    }
    if let Some(serverless_appsec_enabled) = &yaml_config.serverless_appsec_enabled {
        config
            .serverless_appsec_enabled
            .clone_from(serverless_appsec_enabled);
    }
    if yaml_config.extension_version.is_some() {
        config
            .extension_version
            .clone_from(&yaml_config.extension_version);
    }
}

#[derive(Debug, PartialEq, Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct YamlConfigSource {
    pub path: PathBuf,
}

impl ConfigSource for YamlConfigSource {
    fn load(&self, config: &mut Config) -> Result<(), ConfigError> {
        let figment = Figment::new().merge(Yaml::file(self.path.clone()));

        match figment.extract::<YamlConfig>() {
            Ok(yaml_config) => merge_config(config, &yaml_config),
            Err(e) => {
                return Err(ConfigError::ParseError(format!(
                    "Failed to parse config from yaml file: {e}, using default config."
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use crate::config::get_config;

    #[test]
    fn test_otlp_config_receiver_protocols_http_endpoint() {
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

            let config = get_config(Path::new("")).expect("should parse config");

            assert_eq!(
                config.otlp_config_receiver_protocols_http_endpoint,
                Some("0.0.0.0:4318".to_string())
            );

            Ok(())
        });
    }

    #[test]
    fn test_parse_additional_endpoints_from_yaml() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r#"
additional_endpoints:
  "https://app.datadoghq.com":
    - apikey2
    - apikey3
  "https://app.datadoghq.eu":
    - apikey4
"#,
            )?;

            let config = get_config(Path::new("")).expect("should parse config");
            let mut expected = HashMap::new();
            expected.insert(
                "https://app.datadoghq.com".to_string(),
                vec!["apikey2".to_string(), "apikey3".to_string()],
            );
            expected.insert(
                "https://app.datadoghq.eu".to_string(),
                vec!["apikey4".to_string()],
            );
            assert_eq!(config.additional_endpoints, expected);
            Ok(())
        });
    }
}
