use chrono::{DateTime, Utc};
use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::S_TO_NS,
    triggers::{Trigger, DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_TAG},
};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct EventBridgeEvent {
    pub id: String,
    pub version: String,
    pub account: String,
    pub time: DateTime<Utc>,
    pub region: String,
    pub resources: Vec<String>,
    pub source: String,
    #[serde(rename = "detail-type")]
    pub detail_type: String,
    pub detail: Value,
    #[serde(rename = "replay-name")]
    pub replay_name: Option<String>,
}

impl Trigger for EventBridgeEvent {
    fn new(payload: Value) -> Option<Self> {
        match serde_json::from_value(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize EventBridge Event: {}", e);
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        payload.get("detail-type").is_some()
            && payload
                .get("source")
                .and_then(Value::as_str)
                .map_or(false, |s| s != "aws.events")
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span) {
        // EventBridge events have a timestamp resolution in seconds
        let start_time = self
            .time
            .timestamp_nanos_opt()
            .unwrap_or((self.time.timestamp_millis() as f64 * S_TO_NS) as i64);

        // todo: service mapping and peer service
        let service_name = "eventbridge";

        span.name = String::from("aws.eventbridge");
        span.service = service_name.to_string();
        span.resource.clone_from(&self.source);
        span.r#type = String::from("web");
        span.start = start_time;
        span.meta.extend(HashMap::from([
            ("operation_name".to_string(), "aws.eventbridge".to_string()),
            ("detail_type".to_string(), self.detail_type.clone()),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "eventbridge".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.source.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        if let Ok(detail) = serde_json::from_value::<HashMap<String, Value>>(self.detail.clone()) {
            if let Some(carrier) = detail.get(DATADOG_CARRIER_KEY) {
                return serde_json::from_value(carrier.clone()).unwrap_or_default();
            }
        }
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result =
            EventBridgeEvent::new(payload).expect("Failed to deserialize into EventBridgeEvent");

        let expected = EventBridgeEvent {
            id: "bd3c8258-8d30-007c-2562-64715b2d0ea8".to_string(),
            version: "0".to_string(),
            account: "601427279990".to_string(),
            time: DateTime::parse_from_rfc3339("2022-01-24T16:00:10Z")
                .expect("Failed to parse time")
                .with_timezone(&Utc),
            region: "eu-west-1".to_string(),
            resources: vec![],
            source: "my.event".to_string(),
            detail_type: "UserSignUp".to_string(),
            detail: serde_json::json!({
                "hello": "there",
                "_datadog": {
                    "x-datadog-trace-id": "5827606813695714842",
                    "x-datadog-parent-id": "4726693487091824375",
                    "x-datadog-sampled": "1",
                    "x-datadog-sampling-priority": "1"
                }
            }),
            replay_name: None,
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize EventBridgeEvent");

        assert!(EventBridgeEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize EventBridgeEvent");
        assert!(!EventBridgeEvent::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            EventBridgeEvent::new(payload).expect("Failed to deserialize into EventBridgeEvent");

        let mut span = Span::default();
        event.enrich_span(&mut span);

        let expected = serde_json::from_str(&read_json_file("eventbridge_span.json"))
            .expect("Failed to deserialize into Span");
        assert_eq!(span, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = EventBridgeEvent::new(payload).expect("Failed to deserialize EventBridgeEvent");
        assert_eq!(event.get_arn("us-east-1"), "my.event");
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            EventBridgeEvent::new(payload).expect("Failed to deserialize EventBridge Event");
        let carrier = event.get_carrier();

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "5827606813695714842".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "4726693487091824375".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
            ("x-datadog-sampled".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }
}
