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
                Value::Number(n) => n.as_u64().and_then(|n| u8::try_from(n).ok()),
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

/// Returns true if the `headers` JSON contains a trace context header key.
/// This performs a lightweight scan of the raw JSON structure without decoding
/// header values or allocating intermediate collections.
fn headers_has_trace_context(headers: &Value) -> bool {
    // The `headers` field may be either:
    // - an array of header entries
    // - an object with numeric string keys mapping to header entries
    let iter: Box<dyn Iterator<Item = &Value> + '_> = match headers {
        Value::Array(arr) => Box::new(arr.iter()),
        Value::Object(obj) => Box::new(obj.values()),
        _ => return false,
    };

    for entry in iter {
        if let Value::Object(header_map) = entry {
            for key in header_map.keys() {
                if key.eq_ignore_ascii_case("x-datadog-trace-id")
                    || key.eq_ignore_ascii_case("traceparent")
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Scans all records in the records map and returns the `(topic_key, record_value)` of the first
/// record whose headers contain a tracecontext key. Returns `None` if none found.
fn find_record_with_trace_context(
    records_map: &serde_json::Map<String, Value>,
) -> Option<(String, Value)> {
    for (key, group) in records_map {
        match group {
            Value::Array(arr) => {
                for record in arr {
                    if let Some(headers) = record.get("headers")
                        && headers_has_trace_context(headers)
                    {
                        return Some((key.clone(), record.clone()));
                    }
                }
            }
            Value::Object(obj) => {
                for record in obj.values() {
                    if let Some(headers) = record.get("headers")
                        && headers_has_trace_context(headers)
                    {
                        return Some((key.clone(), record.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Decodes an MSK record's `headers` field into a `HashMap<String, String>` by converting
/// each header's byte values to a UTF-8 string. The `headers` field may be either a JSON
/// array or a JSON object with numeric string keys, one entry per Kafka header, ordered by index.
fn headers_to_string_map(headers: &Value) -> HashMap<String, String> {
    let mut carrier = HashMap::new();

    match headers {
        Value::Array(arr) => {
            for entry in arr {
                if let Value::Object(header_map) = entry {
                    for (key, val) in header_map {
                        if let Some(bytes) = bytes_from_header_value(val)
                            && let Ok(s) = String::from_utf8(bytes)
                        {
                            carrier.insert(key.to_lowercase(), s);
                        }
                    }
                }
            }
        }
        // Object format: numeric string keys are just ordering artifacts from the Java runtime;
        // insertion order into the HashMap doesn't matter so no sort needed.
        Value::Object(obj) => {
            for entry in obj.values() {
                if let Value::Object(header_map) = entry {
                    for (key, val) in header_map {
                        if let Some(bytes) = bytes_from_header_value(val)
                            && let Ok(s) = String::from_utf8(bytes)
                        {
                            carrier.insert(key.to_lowercase(), s);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    carrier
}

impl Trigger for MSKEvent {
    fn new(mut payload: Value) -> Option<Self> {
        // We only need one record: prefer the first one carrying Datadog trace context so we can
        // propagate the trace, falling back to the very first record otherwise. Records may be
        // delivered as a JSON object with numeric string keys; normalize to a single-element array
        // before deserializing.
        let chosen = payload
            .get("records")
            .and_then(Value::as_object)
            .and_then(find_record_with_trace_context);

        if let Some((chosen_key, chosen_record)) = chosen {
            let records_map = payload.get_mut("records").and_then(Value::as_object_mut)?;
            records_map.retain(|k, _| k == &chosen_key);
            if let Some(entry) = records_map.get_mut(&chosen_key) {
                *entry = Value::Array(vec![chosen_record]);
            }
        } else {
            // Fallback: no record with Datadog trace context; normalize to the very first record
            // without cloning the full record payload.
            let records_map = payload.get_mut("records").and_then(Value::as_object_mut)?;
            let first_key = records_map.keys().next()?.to_owned();
            records_map.retain(|k, _| k == &first_key);
            if let Some(group) = records_map.get_mut(&first_key) {
                match group {
                    Value::Array(arr) => {
                        if !arr.is_empty() {
                            // Move the first record out, drop the rest.
                            let first = arr.swap_remove(0);
                            arr.clear();
                            arr.push(first);
                        }
                    }
                    Value::Object(obj) => {
                        if let Some((subkey, val)) = obj.iter_mut().next() {
                            // Move the first record out under its original key, drop the rest.
                            let first = std::mem::take(val);
                            let subkey_cloned = subkey.clone();
                            obj.clear();
                            obj.insert(subkey_cloned, first);
                        }
                    }
                    _ => {
                        // Non-array and non-object groups are left as-is, but only the first
                        // outer key is retained above.
                    }
                }
            }
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
            .map_or_else(HashMap::new, |record| {
                headers_to_string_map(&record.headers)
            })
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
        assert_eq!(
            record.headers.as_object().map(serde_json::Map::len),
            Some(6)
        );
    }

    #[test]
    fn test_is_match_with_headers() {
        let json = read_json_file("msk_event_with_headers.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");

        assert!(MSKEvent::is_match(&payload));
    }

    #[test]
    fn test_new_prefers_record_with_trace_context() {
        // Two records in topic1: first has no headers, second has x-datadog-trace-id.
        // [49, 50, 51] = ASCII "123"
        let payload = serde_json::json!({
            "eventSource": "aws:kafka",
            "eventSourceArn": "arn:aws:kafka:us-east-1:123456789012:cluster/demo-cluster/751d2973-a626-431c-9d4e-d7975eb44dd7-2",
            "bootstrapServers": "b-1.demo-cluster.a1bcde.c1.kafka.us-east-1.amazonaws.com:9092",
            "records": {
                "topic1": [
                    {
                        "topic": "topic1", "partition": 0, "offset": 100,
                        "timestamp": 1000.0, "timestampType": "CREATE_TIME",
                        "key": null, "value": null,
                        "headers": []
                    },
                    {
                        "topic": "topic1", "partition": 0, "offset": 101,
                        "timestamp": 2000.0, "timestampType": "CREATE_TIME",
                        "key": null, "value": null,
                        "headers": [{"x-datadog-trace-id": [49, 50, 51]}]
                    }
                ]
            }
        });

        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");
        let carrier = event.get_carrier();
        assert_eq!(
            carrier.get("x-datadog-trace-id").map(String::as_str),
            Some("123"),
            "Should pick the record with trace context, not the first one"
        );
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
