use std::collections::HashMap;

use crate::config::{deserialize_apm_replace_rules, deserialize_processing_rules, ProcessingRule};
use datadog_trace_obfuscation::replacer::ReplaceRule;
use serde::Deserialize;
use serde_aux::field_attributes::deserialize_bool_from_anything;
use serde_json::Value;

/// `Config` is a struct that represents some of the fields in the `datadog.yaml` file.
///
/// It is used to deserialize the `datadog.yaml` file into a struct that can be merged with the `Config` struct.
#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct Config {
    pub logs_config: LogsConfig,
    pub apm_config: ApmConfig,
    pub proxy: ProxyConfig,
    pub otlp_config: OtlpConfig,
}

/// Logs Config
///

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct LogsConfig {
    #[serde(deserialize_with = "deserialize_processing_rules")]
    pub processing_rules: Option<Vec<ProcessingRule>>,
}

/// APM Config
///

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct ApmConfig {
    #[serde(deserialize_with = "deserialize_apm_replace_rules")]
    pub replace_tags: Option<Vec<ReplaceRule>>,
    pub obfuscation: Option<ApmObfuscation>,
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
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub remove_query_string: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub remove_paths_with_digits: bool,
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

/// OTLP Config
///

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpConfig {
    pub receiver: OtlpReceiverConfig,
    pub traces: OtlpTracesConfig,

    // NOT SUPPORTED
    pub metrics: Option<Value>,
    pub logs: Option<Value>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverConfig {
    pub protocols: OtlpReceiverProtocolsConfig,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverProtocolsConfig {
    pub http: OtlpReceiverHttpConfig,

    // NOT SUPPORTED
    pub grpc: Option<Value>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverHttpConfig {
    pub endpoint: String,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpTracesConfig {
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub enabled: bool,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub span_name_as_resource_name: bool,
    pub span_name_remappings: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_bool_from_anything")]
    pub ignore_missing_datadog_fields: bool,

    // NOT SUPORTED
    pub probabilistic_sampler: Option<Value>,
}
