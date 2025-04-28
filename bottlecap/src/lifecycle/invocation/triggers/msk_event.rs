use crate::lifecycle::invocation::processor::MS_TO_NS;
use crate::lifecycle::invocation::triggers::{
    ServiceNameResolver, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
};
use datadog_trace_protobuf::pb::Span;
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
}

impl Trigger for MSKEvent {
    fn new(mut payload: Value) -> Option<Self> {
        // We only care about the first item in the first record, so drop the others before deserializing.
        let mut key_to_keep: Option<String> = None;

        if let Some(records_map) = payload.get_mut("records").and_then(Value::as_object_mut) {
            if let Some((first_key, first_value_ref)) = records_map.iter_mut().next() {
                let key_copy = first_key.clone();
                if let Some(arr) = first_value_ref.as_array_mut() {
                    arr.truncate(1);
                    key_to_keep = Some(key_copy);
                }
            }

            if let Some(ref key) = key_to_keep {
                records_map.retain(|k, _| k == key);
            } else {
                records_map.clear();
            }
        }

        match serde_json::from_value::<Self>(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                println!("[bottlecap] Failed to deserialize modified MSKEvent: {e}");
                debug!("Failed to deserialize modified MSKEvent: {e}");
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        payload
            .get("records")
            .and_then(Value::as_object)
            .and_then(|map| map.values().next())
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .map_or(false, |rec| rec.get("topic").is_some())
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span, service_mapping: &HashMap<String, String>) {
        debug!("Enriching an Inferred Span for an MSK event");

        span.name = String::from("aws.msk");
        span.service = self.resolve_service_name(service_mapping, "msk");
        span.r#type = String::from("web");

        let first_value = self.records.values().find_map(|arr| arr.first());
        if let Some(first_value) = first_value {
            span.resource.clone_from(&first_value.topic);
            span.start = (first_value.timestamp * MS_TO_NS) as i64;
            span.meta.extend([
                ("operation_name".to_string(), String::from("aws.msk")),
                ("topic".to_string(), first_value.topic.to_string()),
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
        self.event_source_arn.to_string()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        HashMap::new()
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
            timestamp: 1745846213022.0,
        };
        let mut expected_records = HashMap::new();
        expected_records.insert(String::from("topic1"), vec![record]);

        let expected = MSKEvent {
            event_source: String::from("aws:kafka"),
            event_source_arn: String::from("arn:aws:kafka:us-east-1:123456789012:cluster/demo-cluster/751d2973-a626-431c-9d4e-d7975eb44dd7-2"),
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
        event.enrich_span(&mut span, &service_mapping);

        assert_eq!(span.name, "aws.msk");
        assert_eq!(span.service, "msk");
        assert_eq!(span.r#type, "web");
        assert_eq!(span.resource, "topic1");
        assert_eq!(span.start, 1745846213022000128);

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
    fn test_resolve_service_name() {
        let json = read_json_file("msk_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = MSKEvent::new(payload).expect("Failed to deserialize MSKEvent");

        // Priority is given to the specific key
        let specific_service_mapping = HashMap::from([
            ("demo-cluster".to_string(), "specific-service".to_string()),
            ("lambda_msk".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(&specific_service_mapping, "msk"),
            "specific-service"
        );

        let generic_service_mapping =
            HashMap::from([("lambda_msk".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(&generic_service_mapping, "msk"),
            "generic-service"
        );
    }
}
