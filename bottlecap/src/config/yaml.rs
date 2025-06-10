use std::collections::HashMap;

use crate::config::additional_endpoints::deserialize_additional_endpoints;
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
    pub otlp_config: Option<OtlpConfig>,
    #[serde(deserialize_with = "deserialize_additional_endpoints")]
    pub additional_endpoints: HashMap<String, Vec<String>>,
}

impl Config {
    #[must_use]
    pub fn otlp_config_receiver_protocols_http_endpoint(&self) -> Option<&str> {
        self.otlp_config
            .as_ref()?
            .receiver
            .as_ref()?
            .protocols
            .as_ref()?
            .http
            .as_ref()?
            .endpoint
            .as_deref()
    }

    #[must_use]
    pub fn otlp_config_receiver_protocols_grpc(&self) -> Option<&Value> {
        self.otlp_config
            .as_ref()?
            .receiver
            .as_ref()?
            .protocols
            .as_ref()?
            .grpc
            .as_ref()
    }

    #[must_use]
    pub fn otlp_config_traces_enabled(&self) -> bool {
        self.otlp_config.as_ref().is_some_and(|otlp_config| {
            otlp_config
                .traces
                .as_ref()
                .is_some_and(|traces| traces.enabled)
        })
    }

    #[must_use]
    pub fn otlp_config_traces_ignore_missing_datadog_fields(&self) -> bool {
        self.otlp_config.as_ref().is_some_and(|otlp_config| {
            otlp_config
                .traces
                .as_ref()
                .is_some_and(|traces| traces.ignore_missing_datadog_fields)
        })
    }

    #[must_use]
    pub fn otlp_config_traces_span_name_as_resource_name(&self) -> bool {
        self.otlp_config.as_ref().is_some_and(|otlp_config| {
            otlp_config
                .traces
                .as_ref()
                .is_some_and(|traces| traces.span_name_as_resource_name)
        })
    }

    #[must_use]
    pub fn otlp_config_traces_span_name_remappings(&self) -> HashMap<String, String> {
        self.otlp_config
            .as_ref()
            .and_then(|otlp_config| otlp_config.traces.as_ref())
            .map(|traces| traces.span_name_remappings.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn otlp_config_traces_probabilistic_sampler(&self) -> Option<&Value> {
        self.otlp_config
            .as_ref()
            .and_then(|otlp_config| otlp_config.traces.as_ref())
            .and_then(|traces| traces.probabilistic_sampler.as_ref())
    }

    #[must_use]
    pub fn otlp_config_logs(&self) -> Option<&Value> {
        self.otlp_config
            .as_ref()
            .and_then(|otlp_config| otlp_config.logs.as_ref())
    }
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
    pub receiver: Option<OtlpReceiverConfig>,
    pub traces: Option<OtlpTracesConfig>,

    // NOT SUPPORTED
    pub metrics: Option<Value>,
    pub logs: Option<Value>,
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
    pub grpc: Option<Value>,
}

#[derive(Debug, PartialEq, Deserialize, Clone, Default)]
#[serde(default)]
#[allow(clippy::module_name_repetitions)]
pub struct OtlpReceiverHttpConfig {
    pub endpoint: Option<String>,
}

impl Default for OtlpTracesConfig {
    fn default() -> Self {
        Self {
            enabled: true, // Default this to true
            span_name_as_resource_name: false,
            span_name_remappings: HashMap::new(),
            ignore_missing_datadog_fields: false,
            probabilistic_sampler: None,
        }
    }
}

#[derive(Debug, PartialEq, Deserialize, Clone)]
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
