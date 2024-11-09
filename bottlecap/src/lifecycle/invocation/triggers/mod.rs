use std::{collections::HashMap, hash::BuildHasher};

use base64::{engine::general_purpose, Engine};
use datadog_trace_protobuf::pb::Span;
use serde::{ser::SerializeMap, Serializer};
use serde_json::Value;

pub mod api_gateway_http_event;
pub mod api_gateway_rest_event;
pub mod event_bridge_event;
pub mod dynamodb_event;
pub mod s3_event;
pub mod sns_event;
pub mod sqs_event;

pub const DATADOG_CARRIER_KEY: &str = "_datadog";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_TAG: &str = "function_trigger.event_source";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG: &str = "function_trigger.event_source_arn";

pub trait Trigger {
    fn new(payload: Value) -> Option<Self>
    where
        Self: Sized;
    fn is_match(payload: &Value) -> bool
    where
        Self: Sized;
    fn enrich_span(&self, span: &mut Span);
    fn get_tags(&self) -> HashMap<String, String>;
    fn get_arn(&self, region: &str) -> String;
    fn get_carrier(&self) -> HashMap<String, String>;
    fn is_async(&self) -> bool;
}

#[must_use]
pub fn get_aws_partition_by_region(region: &str) -> String {
    match region {
        r if r.starts_with("us-gov-") => "aws-us-gov".to_string(),
        r if r.starts_with("cn-") => "aws-cn".to_string(),
        _ => "aws".to_string(),
    }
}

#[must_use]
pub fn base64_to_string(base64_string: &str) -> String {
    let bytes = general_purpose::STANDARD
        .decode(base64_string)
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).to_string()
}

/// Serialize a `HashMap` with lowercase keys
///
pub fn lowercase_key<S, H>(
    map: &HashMap<String, String, H>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    H: BuildHasher,
{
    let mut map_serializer = serializer.serialize_map(Some(map.len()))?;
    for (key, value) in map {
        map_serializer.serialize_entry(&key.to_lowercase(), value)?;
    }
    map_serializer.end()
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
