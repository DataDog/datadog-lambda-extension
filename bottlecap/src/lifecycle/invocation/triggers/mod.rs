use std::collections::HashMap;

use crate::traces::spanpointers::SpanPointer;
use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub mod api_gateway_http_event;
pub mod api_gateway_rest_event;
pub mod dynamodb_event;
pub mod event_bridge_event;
pub mod kinesis_event;
pub mod lambda_function_url_event;
pub mod s3_event;
pub mod sns_event;
pub mod sqs_event;
pub mod step_function_event;

pub const DATADOG_CARRIER_KEY: &str = "_datadog";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_TAG: &str = "function_trigger.event_source";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG: &str = "function_trigger.event_source_arn";

/// Resolves the service name for a given trigger depending on
/// service mapping configuration.
pub trait ServiceNameResolver {
    /// Get the specific service name for this trigger type, it will
    /// be used as a key to resolve the service name
    fn get_specific_identifier(&self) -> String;

    /// Get the generic service mapping key for the trigger
    fn get_generic_identifier(&self) -> &'static str;
}

pub trait Trigger: ServiceNameResolver {
    fn new(payload: Value) -> Option<Self>
    where
        Self: Sized;
    fn is_match(payload: &Value) -> bool
    where
        Self: Sized;
    fn enrich_span(&self, span: &mut Span, service_mapping: &HashMap<String, String>);
    fn get_tags(&self) -> HashMap<String, String>;
    fn get_arn(&self, region: &str) -> String;
    fn get_carrier(&self) -> HashMap<String, String>;
    fn is_async(&self) -> bool;

    /// Default implementation for service name resolution
    fn resolve_service_name(
        &self,
        service_mapping: &HashMap<String, String>,
        fallback: &str,
    ) -> String {
        service_mapping
            .get(&self.get_specific_identifier())
            .or_else(|| service_mapping.get(self.get_generic_identifier()))
            .unwrap_or(&fallback.to_string())
            .to_string()
    }

    /// Default implementation for adding span pointers
    fn get_span_pointers(&self) -> Option<Vec<SpanPointer>> {
        None
    }
}

#[must_use]
pub fn get_aws_partition_by_region(region: &str) -> String {
    match region {
        r if r.starts_with("us-gov-") => "aws-us-gov".to_string(),
        r if r.starts_with("cn-") => "aws-cn".to_string(),
        _ => "aws".to_string(),
    }
}

/// Serialize a `HashMap` with lowercase keys
///
pub fn lowercase_key<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    let map = HashMap::<String, V>::deserialize(deserializer)?;
    Ok(map
        .into_iter()
        .map(|(key, value)| (key.to_lowercase(), value))
        .collect())
}

#[cfg(test)]
pub mod test_utils {
    use std::fs;
    use std::path::PathBuf;

    #[must_use]
    pub fn read_json_file(file_name: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/payloads");
        path.push(file_name);
        fs::read_to_string(path).expect("Failed to read file")
    }
}
