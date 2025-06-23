use std::{collections::HashMap, path::PathBuf};

use crate::{
    config::{
        additional_endpoints::deserialize_additional_endpoints,
        deserialize_apm_replace_rules, deserialize_key_value_pairs,
        deserialize_optional_bool_from_anything, deserialize_processing_rules,
        deserialize_string_or_int,
        flush_strategy::FlushStrategy,
        log_level::LogLevel,
        service_mapping::deserialize_service_mapping,
        trace_propagation_style::{deserialize_trace_propagation_style, TracePropagationStyle},
        Config, ConfigError, ConfigSource, ProcessingRule,
    },
    merge_hashmap, merge_option, merge_option_to_value, merge_string, merge_vec,
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
    // Basic fields
    merge_string!(config, yaml_config, site);
    merge_string!(config, yaml_config, api_key);
    merge_option_to_value!(config, yaml_config, log_level);
    merge_option_to_value!(config, yaml_config, flush_timeout);

    // Unified Service Tagging
    merge_option!(config, yaml_config, env);
    merge_option!(config, yaml_config, service);
    merge_option!(config, yaml_config, version);
    merge_hashmap!(config, yaml_config, tags);

    // Proxy
    merge_option!(config, proxy_https, yaml_config.proxy, https);
    merge_option_to_value!(config, proxy_no_proxy, yaml_config.proxy, no_proxy);
    merge_option!(config, yaml_config, http_protocol);

    // Endpoints
    merge_hashmap!(config, yaml_config, additional_endpoints);
    merge_string!(config, yaml_config, dd_url);

    // Logs
    merge_string!(
        config,
        logs_config_logs_dd_url,
        yaml_config.logs_config,
        logs_dd_url
    );
    merge_option!(
        config,
        logs_config_processing_rules,
        yaml_config.logs_config,
        processing_rules
    );
    merge_option_to_value!(
        config,
        logs_config_use_compression,
        yaml_config.logs_config,
        use_compression
    );
    merge_option_to_value!(
        config,
        logs_config_compression_level,
        yaml_config.logs_config,
        compression_level
    );

    // APM
    merge_hashmap!(config, yaml_config, service_mapping);
    merge_option_to_value!(config, yaml_config, appsec_enabled);
    merge_string!(config, apm_dd_url, yaml_config.apm_config, apm_dd_url);
    merge_option!(
        config,
        apm_replace_tags,
        yaml_config.apm_config,
        replace_tags
    );

    // Not using the macro here because we need to call a method on the struct
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

    merge_vec!(config, apm_features, yaml_config.apm_config, features);

    // Trace Propagation
    merge_vec!(config, yaml_config, trace_propagation_style);
    merge_vec!(config, yaml_config, trace_propagation_style_extract);
    merge_option_to_value!(config, yaml_config, trace_propagation_extract_first);
    merge_option_to_value!(config, yaml_config, trace_propagation_http_baggage_enabled);

    // OTLP
    if let Some(otlp_config) = &yaml_config.otlp_config {
        // Traces

        // Not using macros in some cases because we need to call a method on the struct
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
            merge_option!(
                config,
                otlp_config_traces_probabilistic_sampler_sampling_percentage,
                probabilistic_sampler,
                sampling_percentage
            );
        }

        // Receiver
        let receiver_protocols_http_endpoint = otlp_config.receiver_protocols_http_endpoint();
        if receiver_protocols_http_endpoint.is_some() {
            config
                .otlp_config_receiver_protocols_http_endpoint
                .clone_from(&receiver_protocols_http_endpoint);
        }

        if let Some(receiver_protocols_grpc) = otlp_config.receiver_protocols_grpc() {
            merge_option!(
                config,
                otlp_config_receiver_protocols_grpc_endpoint,
                receiver_protocols_grpc,
                endpoint
            );
            merge_option!(
                config,
                otlp_config_receiver_protocols_grpc_transport,
                receiver_protocols_grpc,
                transport
            );
            merge_option!(
                config,
                otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib,
                receiver_protocols_grpc,
                max_recv_msg_size_mib
            );
        }

        // Metrics
        if let Some(metrics) = &otlp_config.metrics {
            merge_option_to_value!(config, otlp_config_metrics_enabled, metrics, enabled);
            merge_option_to_value!(
                config,
                otlp_config_metrics_resource_attributes_as_tags,
                metrics,
                resource_attributes_as_tags
            );
            merge_option_to_value!(
                config,
                otlp_config_metrics_instrumentation_scope_metadata_as_tags,
                metrics,
                instrumentation_scope_metadata_as_tags
            );
            merge_option!(
                config,
                otlp_config_metrics_tag_cardinality,
                metrics,
                tag_cardinality
            );
            merge_option!(config, otlp_config_metrics_delta_ttl, metrics, delta_ttl);
            if let Some(histograms) = &metrics.histograms {
                merge_option_to_value!(
                    config,
                    otlp_config_metrics_histograms_send_count_sum_metrics,
                    histograms,
                    send_count_sum_metrics
                );
                merge_option_to_value!(
                    config,
                    otlp_config_metrics_histograms_send_aggregation_metrics,
                    histograms,
                    send_aggregation_metrics
                );
                merge_option!(
                    config,
                    otlp_config_metrics_histograms_mode,
                    histograms,
                    mode
                );
            }
            if let Some(sums) = &metrics.sums {
                merge_option!(
                    config,
                    otlp_config_metrics_sums_cumulative_monotonic_mode,
                    sums,
                    cumulative_monotonic_mode
                );
                merge_option!(
                    config,
                    otlp_config_metrics_sums_initial_cumulativ_monotonic_value,
                    sums,
                    initial_cumulative_monotonic_value
                );
            }
            if let Some(summaries) = &metrics.summaries {
                merge_option!(config, otlp_config_metrics_summaries_mode, summaries, mode);
            }
        }

        // Logs
        if let Some(logs) = &otlp_config.logs {
            merge_option_to_value!(config, otlp_config_logs_enabled, logs, enabled);
        }
    }

    // AWS Lambda
    merge_string!(config, yaml_config, api_key_secret_arn);
    merge_string!(config, yaml_config, kms_api_key);
    merge_option_to_value!(config, yaml_config, serverless_logs_enabled);
    merge_option_to_value!(config, yaml_config, serverless_flush_strategy);
    merge_option_to_value!(config, yaml_config, enhanced_metrics);
    merge_option_to_value!(config, yaml_config, lambda_proc_enhanced_metrics);
    merge_option_to_value!(config, yaml_config, capture_lambda_payload);
    merge_option_to_value!(config, yaml_config, capture_lambda_payload_max_depth);
    merge_option_to_value!(config, yaml_config, serverless_appsec_enabled);
    merge_option!(config, yaml_config, extension_version);
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
