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
        // TODO
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
