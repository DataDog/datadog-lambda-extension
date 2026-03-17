use libdd_trace_protobuf::pb::Span as DatadogSpan;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use serde_json::Value;
use std::{error::Error, sync::Arc};

use crate::{config::Config, otlp::transform::otel_resource_spans_to_dd_spans};

/// Fields that contain 64-bit nanosecond timestamps and need flexible deserialization.
/// Per proto3 JSON spec, these should be string-encoded, but some SDKs send integers
/// or even objects like {"low": ..., "high": ...}.
const TIMESTAMP_FIELDS: &[&str] = &[
    "startTimeUnixNano",
    "endTimeUnixNano",
    "timeUnixNano",
    "observedTimeUnixNano",
];

/// Recursively normalizes timestamp fields in a JSON value.
/// Converts integer timestamps to strings and handles the {"low": ..., "high": ...}
/// object format from older/buggy OpenTelemetry JS SDKs.
fn normalize_timestamps(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if TIMESTAMP_FIELDS.contains(&key.as_str()) {
                    normalize_timestamp_value(val);
                } else {
                    normalize_timestamps(val);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                normalize_timestamps(item);
            }
        }
        _ => {}
    }
}

/// Normalizes a single timestamp value to a string.
/// Handles:
/// - String: already correct, leave as-is
/// - Integer: convert to string
/// - Object {"low": n, "high": m}: reconstruct 64-bit value and convert to string
fn normalize_timestamp_value(value: &mut Value) {
    match value {
        Value::Number(n) => {
            // Integer timestamp - convert to string
            if let Some(i) = n.as_u64() {
                *value = Value::String(i.to_string());
            } else if let Some(i) = n.as_i64() {
                *value = Value::String(i.to_string());
            }
        }
        Value::Object(map) => {
            // Handle {"low": n, "high": m} format from buggy JS SDKs
            // This represents a 64-bit integer split into two 32-bit parts
            let low_val = map.get("low").and_then(Value::as_u64);
            let high_val = map.get("high").and_then(Value::as_u64);
            if let (Some(low), Some(high)) = (low_val, high_val) {
                // Reconstruct the 64-bit value: high << 32 | low
                let timestamp = (high << 32) | (low & 0xFFFF_FFFF);
                *value = Value::String(timestamp.to_string());
            }
        }
        // String or other types: nothing to do
        _ => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OtlpEncoding {
    Protobuf,
    Json,
}

impl OtlpEncoding {
    #[must_use]
    pub fn from_content_type(content_type: Option<&str>) -> Self {
        match content_type {
            Some(ct) if ct.starts_with("application/json") => OtlpEncoding::Json,
            _ => OtlpEncoding::Protobuf,
        }
    }

    #[must_use]
    pub fn content_type(&self) -> &'static str {
        match self {
            OtlpEncoding::Json => "application/json",
            OtlpEncoding::Protobuf => "application/x-protobuf",
        }
    }
}

#[derive(Clone)]
pub struct Processor {
    config: Arc<Config>,
}

impl Processor {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub fn process(
        &self,
        body: &[u8],
        encoding: OtlpEncoding,
    ) -> Result<Vec<Vec<DatadogSpan>>, Box<dyn Error>> {
        let request = match encoding {
            OtlpEncoding::Json => {
                let mut json_value: Value = serde_json::from_slice(body)?;
                normalize_timestamps(&mut json_value);
                serde_json::from_value::<ExportTraceServiceRequest>(json_value)?
            }
            OtlpEncoding::Protobuf => ExportTraceServiceRequest::decode(body)?,
        };

        let mut spans: Vec<Vec<DatadogSpan>> = Vec::new();
        for resource_spans in &request.resource_spans {
            spans.extend(otel_resource_spans_to_dd_spans(
                resource_spans,
                self.config.clone(),
            ));
        }

        Ok(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_integer_timestamp_converted_to_string() {
        let mut value = json!({"startTimeUnixNano": 1_581_452_772_000_000_321_u64});
        normalize_timestamps(&mut value);
        assert_eq!(value["startTimeUnixNano"], json!("1581452772000000321"));
    }

    #[test]
    fn test_split_object_timestamp_reconstructed() {
        // Some old JS SDKs send 64-bit ints as {"low": u32, "high": u32}
        let mut value = json!({"startTimeUnixNano": {"low": 1_029_784_000_u64, "high": 395_146_000_u64}});
        normalize_timestamps(&mut value);
        let expected = (395_146_000_u64 << 32) | 1_029_784_000_u64;
        assert_eq!(value["startTimeUnixNano"], json!(expected.to_string()));
    }

    #[test]
    fn test_non_timestamp_integers_unchanged() {
        // Verify we only convert timestamp fields, not all integers
        let mut value = json!({
            "kind": 1,
            "droppedAttributesCount": 5,
            "attributes": [{"value": {"intValue": 42}}],
            "startTimeUnixNano": 12345_u64
        });
        normalize_timestamps(&mut value);

        // These should remain as integers
        assert_eq!(value["kind"], json!(1));
        assert_eq!(value["droppedAttributesCount"], json!(5));
        assert_eq!(value["attributes"][0]["value"]["intValue"], json!(42));
        // Only this should be converted
        assert_eq!(value["startTimeUnixNano"], json!("12345"));
    }

    #[test]
    fn test_nested_event_timestamps_normalized() {
        let mut value = json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "startTimeUnixNano": 100_u64,
                        "endTimeUnixNano": "200",
                        "events": [{"timeUnixNano": 300_u64}]
                    }]
                }]
            }]
        });
        normalize_timestamps(&mut value);

        let span = &value["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        assert_eq!(span["startTimeUnixNano"], json!("100"));
        assert_eq!(span["endTimeUnixNano"], json!("200")); // Already string
        assert_eq!(span["events"][0]["timeUnixNano"], json!("300"));
    }
}
