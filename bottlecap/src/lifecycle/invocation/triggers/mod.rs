use std::{collections::HashMap, hash::BuildHasher};

use datadog_trace_protobuf::pb::Span;
use serde::{ser::SerializeMap, Serializer};
use serde_json::Value;

pub mod api_gateway_http_event;
pub mod api_gateway_rest_event;

pub trait Trigger: Sized {
    fn new(payload: Value) -> Option<Self>;
    fn is_match(payload: &Value) -> bool;
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

    pub fn read_json_file(file_name: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/payloads");
        path.push(file_name);
        fs::read_to_string(path).expect("Failed to read file")
    }
}