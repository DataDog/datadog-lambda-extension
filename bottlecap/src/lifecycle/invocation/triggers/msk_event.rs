use crate::lifecycle::invocation::processor::MS_TO_NS;
use crate::lifecycle::invocation::triggers::{
    FUNCTION_TRIGGER_EVENT_SOURCE_TAG, ServiceNameResolver, Trigger,
};
use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MSKEvent {
    pub event_source: String,
    pub event_source_arn: String,
    pub records: HashMap<String, Vec<MSKRecord>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MSKRecord {
    pub topic: String,
    pub partition: i32,
    pub timestamp: f64,
    #[serde(default)]
    pub headers: Value,
}

/// Decodes a header value into raw bytes. Two formats have been observed:
///
/// - **Array**: elements are integers `[104, 101]` or decimal strings `["49"]`
/// - **Object** with numeric string keys and decimal string values: `{"0":"52","1":"54",...}`
fn bytes_from_header_value(val: &Value) -> Option<Vec<u8>> {
    match val {
        // Array format: elements may be integers `[104, 101]` or decimal strings `["49"]`
        Value::Array(arr) => arr
            .iter()
            .map(|v| match v {
                Value::Number(n) => n.as_u64().map(|n| n as u8),
                Value::String(s) => s.parse::<u8>().ok(),
                _ => None,
            })
            .collect(),
        // Object format with numeric string keys and decimal string values: `{"0":"52","1":"54",...}`
        Value::Object(obj) => {
            let mut pairs: Vec<(u64, u8)> = obj
                .iter()
                .filter_map(|(k, v)| {
                    Some((k.parse::<u64>().ok()?, v.as_str()?.parse::<u8>().ok()?))
                })
                .collect();
            pairs.sort_by_key(|(idx, _)| *idx);
            Some(pairs.into_iter().map(|(_, b)| b).collect())
        }
        _ => None,
    }
}

/// Extracts trace propagation headers from an MSK record's `headers` field into a carrier map.
/// The `headers` field is a JSON object with numeric string keys, one entry per Kafka header.
fn carrier_from_headers(headers: &Value) -> HashMap<String, String> {
    let mut carrier = HashMap::new();

    let entries: Vec<&Value> = match headers {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(obj) => {
            let mut pairs: Vec<(u64, &Value)> = obj
                .iter()
                .filter_map(|(k, v)| k.parse::<u64>().ok().map(|n| (n, v)))
                .collect();
            pairs.sort_by_key(|(n, _)| *n);
            pairs.into_iter().map(|(_, v)| v).collect()
        }
        _ => return carrier,
    };

    for entry in entries {
        if let Value::Object(header_map) = entry {
            for (key, val) in header_map {
                if let Some(bytes) = bytes_from_header_value(val) {
                    if let Ok(s) = String::from_utf8(bytes) {
                        carrier.insert(key.to_lowercase(), s);
                    }
                }
            }
        }
    }

    carrier
}

impl Trigger for MSKEvent {
    fn new(mut payload: Value) -> Option<Self> {
        // We only care about the first item in the first record, so drop the others before
        // deserializing. Records are delivered as a JSON object with numeric string keys;
        // normalize to a single-element array before deserializing.
        if let Some(records_map) = payload.get_mut("records").and_then(Value::as_object_mut) {
            let first_key = records_map.keys().next()?.clone();
            let normalized = match records_map.get(&first_key)? {
                Value::Array(arr) => Value::Array(vec![arr.first()?.clone()]),
                Value::Object(obj) => {
                    // Records delivered as object with numeric string keys: {"0": {...}, "1": {...}, ...}
                    // Take the record with the lowest numeric key.
                    let mut pairs: Vec<(u64, Value)> = obj
                        .iter()
                        .filter_map(|(k, v)| k.parse::<u64>().ok().map(|n| (n, v.clone())))
                        .collect();
                    pairs.sort_by_key(|(n, _)| *n);
                    let (_, first_record) = pairs.into_iter().next()?;
                    Value::Array(vec![first_record])
                }
                _ => return None,
            };
            *records_map = serde_json::Map::from_iter([(first_key, normalized)]);
        }

        match serde_json::from_value::<Self>(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize modified MSKEvent: {e}");
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        let first_record_group = payload
            .get("records")
            .and_then(Value::as_object)
            .and_then(|map| map.values().next());
        let first_record = match first_record_group {
            Some(Value::Array(arr)) => arr.first(),
            Some(Value::Object(obj)) => obj.values().next(),
            _ => return false,
        };
        first_record.is_some_and(|rec| rec.get("topic").is_some())
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(
        &self,
        span: &mut Span,
        service_mapping: &HashMap<String, String>,
        aws_service_representation_enabled: bool,
    ) {
        debug!("Enriching an Inferred Span for an MSK event");

        span.name = String::from("aws.msk");
        span.service = self.resolve_service_name(
            service_mapping,
            &self.get_specific_identifier(),
            "msk",
            aws_service_representation_enabled,
        );
        span.r#type = String::from("web");

        let first_value = self.records.values().find_map(|arr| arr.first());
        if let Some(first_value) = first_value {
            span.resource.clone_from(&first_value.topic);
            span.start = (first_value.timestamp * MS_TO_NS) as i64;
            span.meta.extend([
                ("operation_name".to_string(), String::from("aws.msk")),
                ("topic".to_string(), first_value.topic.clone()),
                ("partition".to_string(), first_value.partition.to_string()),
                ("event_source".to_string(), self.event_source.clone()),
                (
                    "event_source_arn".to_string(),
                    self.event_source_arn.clone(),
                ),
            ]);
        }
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "msk".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.event_source_arn.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        self.records
            .values()
            .find_map(|arr| arr.first())
            .map_or_else(HashMap::new, |record| carrier_from_headers(&record.headers))
    }

    fn is_async(&self) -> bool {
        true
    }
}

impl ServiceNameResolver for MSKEvent {
    fn get_specific_identifier(&self) -> String {
        self.event_source_arn
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_msk"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = MSKEvent::new(payload).expect("Failed to deserialize into MSKEvent");

        let record = MSKRecord {
            topic: String::from("topic1"),
            partition: 0,
            timestamp: 1_745_846_213_022f64,
            headers: Value::Array(vec![]),
        };
        let mut expected_records = HashMap::new();
        expected_records.insert(String::from("topic1"), vec![record]);

        let expected = MSKEvent {
            event_source: String::from("aws:kafka"),
            event_source_arn: String::from(
                "arn:aws:kafka:us-east-1:123456789012:cluster/demo-cluster/751d2973-a626-431c-9d4e-d7975eb44dd7-2",
            ),
            records: expected_records,
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize MSKEvent");

        assert!(MSKEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");
        assert!(!MSKEvent::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");
        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);

        assert_eq!(span.name, "aws.msk");
        assert_eq!(span.service, "demo-cluster");
        assert_eq!(span.r#type, "web");
        assert_eq!(span.resource, "topic1");
        assert_eq!(span.start, 1_745_846_213_022_000_128);

        assert_eq!(
            span.meta,
            HashMap::from([
                ("operation_name".to_string(), "aws.msk".to_string()),
                ("topic".to_string(), "topic1".to_string()),
                ("partition".to_string(), "0".to_string()),
                ("event_source".to_string(), "aws:kafka".to_string()),
                (
                    "event_source_arn".to_string(),
                    "arn:aws:kafka:us-east-1:123456789012:cluster/demo-cluster/751d2973-a626-431c-9d4e-d7975eb44dd7-2".to_string()
                ),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");
        let tags = event.get_tags();

        let expected = HashMap::from([(
            "function_trigger.event_source".to_string(),
            "msk".to_string(),
        )]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:kafka:us-east-1:123456789012:cluster/demo-cluster/751d2973-a626-431c-9d4e-d7975eb44dd7-2"
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");
        let carrier = event.get_carrier();

        let expected = HashMap::new();

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_resolve_service_name_with_representation_enabled() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");

        // Test 1: Specific mapping takes priority
        let specific_service_mapping = HashMap::from([
            ("demo-cluster".to_string(), "specific-service".to_string()),
            ("lambda_msk".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "msk",
                true // aws_service_representation_enabled
            ),
            "specific-service"
        );

        // Test 2: Generic mapping is used when specific not found
        let generic_service_mapping =
            HashMap::from([("lambda_msk".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "msk",
                true // aws_service_representation_enabled
            ),
            "generic-service"
        );

        // Test 3: When no mapping exists, uses instance name (cluster name)
        let empty_mapping = HashMap::new();
        assert_eq!(
            event.resolve_service_name(
                &empty_mapping,
                &event.get_specific_identifier(),
                "msk",
                true // aws_service_representation_enabled
            ),
            event.get_specific_identifier() // instance name
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_disabled() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");

        // Test 1: With specific mapping - still respects mapping
        let specific_service_mapping = HashMap::from([
            ("demo-cluster".to_string(), "specific-service".to_string()),
            ("lambda_msk".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "msk",
                false // aws_service_representation_enabled = false
            ),
            "specific-service"
        );

        // Test 2: With generic mapping - still respects mapping
        let generic_service_mapping =
            HashMap::from([("lambda_msk".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "msk",
                false // aws_service_representation_enabled = false
            ),
            "generic-service"
        );

        // Test 3: When no mapping exists, uses fallback value
        let empty_mapping = HashMap::new();
        assert_eq!(
            event.resolve_service_name(
                &empty_mapping,
                &event.get_specific_identifier(),
                "msk",
                false // aws_service_representation_enabled = false
            ),
            "msk" // fallback value
        );
    }

    #[test]
    fn test_new_with_headers() {
        let json = read_json_file("msk_event_with_headers.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = MSKEvent::new(payload).expect("Failed to deserialize into MSKEvent");

        let record = result
            .records
            .values()
            .find_map(|arr| arr.first())
            .expect("Expected at least one record");
        assert_eq!(record.topic, "demo-topic");
        // headers is an object with 6 entries (2 non-datadog + 4 datadog)
        assert_eq!(record.headers.as_object().map(|o| o.len()), Some(6));
    }

    #[test]
    fn test_get_carrier_with_headers() {
        let json = read_json_file("msk_event_with_headers.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");
        let carrier = event.get_carrier();

        // Datadog headers appear at indices 2-5; non-datadog headers at 0-1 are also decoded
        // but won't be used by the propagator.
        assert_eq!(
            carrier.get("x-datadog-trace-id").map(String::as_str),
            Some("1497116011738644768")
        );
        assert_eq!(
            carrier.get("x-datadog-parent-id").map(String::as_str),
            Some("2239801583077304042")
        );
        assert_eq!(
            carrier
                .get("x-datadog-sampling-priority")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            carrier.get("x-datadog-tags").map(String::as_str),
            Some("_dd.p.dm=-1,_dd.p.tid=699c836500000000")
        );
    }
}
