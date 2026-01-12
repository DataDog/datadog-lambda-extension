use std::time::Duration;
use std::{collections::HashMap, path::PathBuf};

use crate::{
    config::{
        Config, ConfigError, ConfigSource, ProcessingRule,
        additional_endpoints::deserialize_additional_endpoints,
        deserialize_apm_replace_rules, deserialize_key_value_pair_array_to_hashmap,
        deserialize_option_lossless, deserialize_optional_bool_from_anything,
        deserialize_optional_duration_from_microseconds,
        deserialize_optional_duration_from_seconds,
        deserialize_optional_duration_from_seconds_ignore_zero, deserialize_optional_string,
        deserialize_processing_rules, deserialize_string_or_int,
        flush_strategy::FlushStrategy,
        log_level::LogLevel,
        logs_additional_endpoints::LogsAdditionalEndpoint,
        service_mapping::deserialize_service_mapping,
        trace_propagation_style::{TracePropagationStyle, deserialize_trace_propagation_style},
    },
    merge_hashmap, merge_option, merge_option_to_value, merge_string, merge_vec,
};
use figment::{
    Figment,
    providers::{Format, Yaml},
};
use libdd_trace_obfuscation::replacer::ReplaceRule;
use serde::Deserialize;

/// `YamlConfig` is a struct that represents some of the fields in the `datadog.yaml` file.
///
/// It is used to deserialize the `datadog.yaml` file into a struct that can be merged
/// with the `Config` struct.
#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct YamlConfig {
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub site: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub api_key: Option<String>,
    pub log_level: Option<LogLevel>,

    #[serde(deserialize_with = "deserialize_option_lossless")]
    pub flush_timeout: Option<u64>,

    #[serde(deserialize_with = "deserialize_option_lossless")]
    pub compression_level: Option<i32>,

    // Proxy
    pub proxy: ProxyConfig,
    // nit: this should probably be in the endpoints section
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub dd_url: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub http_protocol: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub tls_cert_file: Option<String>,

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
    #[serde(deserialize_with = "deserialize_key_value_pair_array_to_hashmap")]
    pub tags: HashMap<String, String>,

    // Logs
    pub logs_config: LogsConfig,

    // APM
    pub apm_config: ApmConfig,
    #[serde(deserialize_with = "deserialize_service_mapping")]
    pub service_mapping: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_aws_service_representation_enabled: Option<bool>,
    // Trace Propagation
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style: Vec<TracePropagationStyle>,
    #[serde(deserialize_with = "deserialize_trace_propagation_style")]
    pub trace_propagation_style_extract: Vec<TracePropagationStyle>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_propagation_extract_first: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub trace_propagation_http_baggage_enabled: Option<bool>,

    // Metrics
    pub metrics_config: MetricsConfig,

    // OTLP
    pub otlp_config: Option<OtlpConfig>,

    // AWS Lambda
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub api_key_secret_arn: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub kms_api_key: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub serverless_logs_enabled: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub logs_enabled: Option<bool>,
    pub serverless_flush_strategy: Option<FlushStrategy>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub enhanced_metrics: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub lambda_proc_enhanced_metrics: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub capture_lambda_payload: Option<bool>,
    #[serde(deserialize_with = "deserialize_option_lossless")]
    pub capture_lambda_payload_max_depth: Option<u32>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub compute_trace_stats_on_extension: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_duration_from_seconds_ignore_zero")]
    pub api_key_secret_reload_interval: Option<Duration>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub serverless_appsec_enabled: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_string")]
    pub appsec_rules: Option<String>,
    #[serde(deserialize_with = "deserialize_optional_duration_from_microseconds")]
    pub appsec_waf_timeout: Option<Duration>,
    #[serde(deserialize_with = "deserialize_optional_bool_from_anything")]
    pub api_security_enabled: Option<bool>,
    #[serde(deserialize_with = "deserialize_optional_duration_from_seconds")]
    pub api_security_sample_delay: Option<Duration>,
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
    #[serde(deserialize_with = "deserialize_option_lossless")]
    pub compression_level: Option<i32>,
    pub additional_endpoints: Vec<LogsAdditionalEndpoint>,
}

/// Metrics specific config
///
#[derive(Debug, PartialEq, Deserialize, Clone, Copy, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct MetricsConfig {
    #[serde(deserialize_with = "deserialize_option_lossless")]
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
    #[serde(deserialize_with = "deserialize_option_lossless")]
    pub compression_level: Option<i32>,
    pub features: Vec<String>,
    #[serde(deserialize_with = "deserialize_additional_endpoints")]
    pub additional_endpoints: HashMap<String, Vec<String>>,
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
    #[serde(deserialize_with = "deserialize_option_lossless")]
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
    #[serde(deserialize_with = "deserialize_option_lossless")]
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
    #[serde(deserialize_with = "deserialize_option_lossless")]
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

    merge_option_to_value!(config, yaml_config, compression_level);
    // Proxy
    merge_option!(config, proxy_https, yaml_config.proxy, https);
    merge_option_to_value!(config, proxy_no_proxy, yaml_config.proxy, no_proxy);
    merge_option!(config, yaml_config, http_protocol);
    merge_option!(config, yaml_config, tls_cert_file);

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
        yaml_config,
        compression_level
    );
    merge_option_to_value!(
        config,
        logs_config_compression_level,
        yaml_config.logs_config,
        compression_level
    );
    merge_vec!(
        config,
        logs_config_additional_endpoints,
        yaml_config.logs_config,
        additional_endpoints
    );

    merge_option_to_value!(
        config,
        metrics_config_compression_level,
        yaml_config,
        compression_level
    );

    merge_option_to_value!(
        config,
        metrics_config_compression_level,
        yaml_config.metrics_config,
        compression_level
    );

    // APM
    merge_hashmap!(config, yaml_config, service_mapping);
    merge_string!(config, apm_dd_url, yaml_config.apm_config, apm_dd_url);
    merge_option!(
        config,
        apm_replace_tags,
        yaml_config.apm_config,
        replace_tags
    );
    merge_option_to_value!(
        config,
        apm_config_compression_level,
        yaml_config,
        compression_level
    );
    merge_option_to_value!(
        config,
        apm_config_compression_level,
        yaml_config.apm_config,
        compression_level
    );
    merge_hashmap!(
        config,
        apm_additional_endpoints,
        yaml_config.apm_config,
        additional_endpoints
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
    merge_option_to_value!(
        config,
        yaml_config,
        trace_aws_service_representation_enabled
    );

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

    // Handle serverless_logs_enabled with OR logic: if either logs_enabled or serverless_logs_enabled is true, enable logs
    if yaml_config.serverless_logs_enabled.is_some() || yaml_config.logs_enabled.is_some() {
        config.serverless_logs_enabled = yaml_config.serverless_logs_enabled.unwrap_or(false)
            || yaml_config.logs_enabled.unwrap_or(false);
    }

    merge_option_to_value!(config, yaml_config, serverless_flush_strategy);
    merge_option_to_value!(config, yaml_config, enhanced_metrics);
    merge_option_to_value!(config, yaml_config, lambda_proc_enhanced_metrics);
    merge_option_to_value!(config, yaml_config, capture_lambda_payload);
    merge_option_to_value!(config, yaml_config, capture_lambda_payload_max_depth);
    merge_option_to_value!(config, yaml_config, compute_trace_stats_on_extension);
    merge_option!(config, yaml_config, api_key_secret_reload_interval);
    merge_option_to_value!(config, yaml_config, serverless_appsec_enabled);
    merge_option!(config, yaml_config, appsec_rules);
    merge_option_to_value!(config, yaml_config, appsec_waf_timeout);
    merge_option_to_value!(config, yaml_config, api_security_enabled);
    merge_option_to_value!(config, yaml_config, api_security_sample_delay);
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

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use crate::config::{flush_strategy::PeriodicStrategy, processing_rule::Kind};

    use super::*;

    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_merge_config_overrides_with_yaml_file() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "datadog.yaml",
                r#"
# Basic fields
site: "test-site"
api_key: "test-api-key"
log_level: "debug"
flush_timeout: 42
compression_level: 4
# Proxy
proxy:
  https: "https://proxy.example.com"
  no_proxy: ["localhost", "127.0.0.1"]
dd_url: "https://metrics.datadoghq.com"
http_protocol: "http1"
tls_cert_file: "/opt/ca-cert.pem"

# Endpoints
additional_endpoints:
  "https://app.datadoghq.com":
    - apikey2
    - apikey3
  "https://app.datadoghq.eu":
    - apikey4

# Unified Service Tagging
env: "test-env"
service: "test-service"
version: "1.0.0"
tags:
  - "team:test-team"
  - "project:test-project"

# Logs
logs_config:
  logs_dd_url: "https://logs.datadoghq.com"
  processing_rules:
    - name: "test-exclude"
      type: "exclude_at_match"
      pattern: "test-pattern"
  use_compression: false
  compression_level: 1
  additional_endpoints:
    - api_key: "apikey2"
      Host: "agent-http-intake.logs.datadoghq.com"
      Port: 443
      is_reliable: true

# APM
apm_config:
  apm_dd_url: "https://apm.datadoghq.com"
  replace_tags: []
  obfuscation:
    http:
      remove_query_string: true
      remove_paths_with_digits: true
  compression_level: 2
  features:
    - "enable_otlp_compute_top_level_by_span_kind"
    - "enable_stats_by_span_kind"
  additional_endpoints:
    "https://trace.agent.datadoghq.com":
        - apikey2
        - apikey3
    "https://trace.agent.datadoghq.eu":
        - apikey4

service_mapping: old-service:new-service

# Trace Propagation
trace_propagation_style: "datadog"
trace_propagation_style_extract: "b3"
trace_propagation_extract_first: true
trace_propagation_http_baggage_enabled: true
trace_aws_service_representation_enabled: true

metrics_config:
  compression_level: 3

# OTLP
otlp_config:
  receiver:
    protocols:
      http:
        endpoint: "http://localhost:4318"
      grpc:
        endpoint: "http://localhost:4317"
        transport: "tcp"
        max_recv_msg_size_mib: 4
  traces:
    enabled: false
    span_name_as_resource_name: true
    span_name_remappings:
      "old-span": "new-span"
    ignore_missing_datadog_fields: true
    probabilistic_sampler:
      sampling_percentage: 50
  metrics:
    enabled: true
    resource_attributes_as_tags: true
    instrumentation_scope_metadata_as_tags: true
    tag_cardinality: "low"
    delta_ttl: 3600
    histograms:
      mode: "counters"
      send_count_sum_metrics: true
      send_aggregation_metrics: true
    sums:
      cumulative_monotonic_mode: "to_delta"
      initial_cumulative_monotonic_value: "auto"
    summaries:
      mode: "quantiles"
  logs:
    enabled: true

# AWS Lambda
api_key_secret_arn: "arn:aws:secretsmanager:region:account:secret:datadog-api-key"
kms_api_key: "test-kms-key"
serverless_logs_enabled: false
serverless_flush_strategy: "periodically,60000"
enhanced_metrics: false
lambda_proc_enhanced_metrics: false
capture_lambda_payload: true
capture_lambda_payload_max_depth: 5
compute_trace_stats_on_extension: true
api_key_secret_reload_interval: 0
serverless_appsec_enabled: true
appsec_rules: "/path/to/rules.json"
appsec_waf_timeout: 1000000 # Microseconds
api_security_enabled: false
api_security_sample_delay: 60 # Seconds
"#,
            )?;

            let mut config = Config::default();
            let yaml_config_source = YamlConfigSource {
                path: Path::new("datadog.yaml").to_path_buf(),
            };
            yaml_config_source
                .load(&mut config)
                .expect("Failed to load config");

            let expected_config = Config {
                site: "test-site".to_string(),
                api_key: "test-api-key".to_string(),
                log_level: LogLevel::Debug,
                flush_timeout: 42,
                compression_level: 4,
                proxy_https: Some("https://proxy.example.com".to_string()),
                proxy_no_proxy: vec!["localhost".to_string(), "127.0.0.1".to_string()],
                http_protocol: Some("http1".to_string()),
                tls_cert_file: Some("/opt/ca-cert.pem".to_string()),
                dd_url: "https://metrics.datadoghq.com".to_string(),
                url: String::new(), // doesnt exist in yaml
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
                    name: "test-exclude".to_string(),
                    pattern: "test-pattern".to_string(),
                    kind: Kind::ExcludeAtMatch,
                    replace_placeholder: None,
                }]),
                logs_config_use_compression: false,
                logs_config_compression_level: 1,
                logs_config_additional_endpoints: vec![LogsAdditionalEndpoint {
                    api_key: "apikey2".to_string(),
                    host: "agent-http-intake.logs.datadoghq.com".to_string(),
                    port: 443,
                    is_reliable: true,
                }],
                observability_pipelines_worker_logs_enabled: false,
                observability_pipelines_worker_logs_url: String::default(),
                service_mapping: HashMap::from([(
                    "old-service".to_string(),
                    "new-service".to_string(),
                )]),
                apm_dd_url: "https://apm.datadoghq.com".to_string(),
                apm_replace_tags: Some(vec![]),
                apm_config_obfuscation_http_remove_query_string: true,
                apm_config_obfuscation_http_remove_paths_with_digits: true,
                apm_config_compression_level: 2,
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
                metrics_config_compression_level: 3,
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
                api_key_ssm_arn: String::default(),
                serverless_logs_enabled: false,
                serverless_flush_strategy: FlushStrategy::Periodically(PeriodicStrategy {
                    interval: 60000,
                }),
                enhanced_metrics: false,
                lambda_proc_enhanced_metrics: false,
                capture_lambda_payload: true,
                capture_lambda_payload_max_depth: 5,
                compute_trace_stats_on_extension: true,
                api_key_secret_reload_interval: None,

                serverless_appsec_enabled: true,
                appsec_rules: Some("/path/to/rules.json".to_string()),
                appsec_waf_timeout: Duration::from_secs(1),
                api_security_enabled: false,
                api_security_sample_delay: Duration::from_secs(60),

                apm_filter_tags_require: None,
                apm_filter_tags_reject: None,
                apm_filter_tags_regex_require: None,
                apm_filter_tags_regex_reject: None,
                statsd_metric_namespace: None,
                policy_enabled: false,
                policy_providers: None,
            };

            // Assert that
            assert_eq!(config, expected_config);

            Ok(())
        });
    }
}
