use chrono::{DateTime, Utc};
use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{processor::MS_TO_NS, triggers::Trigger};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct EventBridgeEvent {
    pub id: String,
    pub version: String,
    pub account: String,
    pub time: String,
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
                debug!("Failed to deserialize EventBridgeEvent: {}", e);
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        payload.get("detail-type").is_some()
    }

    fn enrich_span(&self, span: &mut Span) {
        span.name = "aws.eventbridge".to_string();
        // TODO service name fallback value for now, needs service mapping
        span.service = "eventbridge".to_string();
        span.resource.clone_from(&self.source);
        span.r#type = "web".to_string();

        let parsed_date: DateTime<Utc> = self.time.parse().expect("Failed to parse date");
        let start_time = parsed_date.timestamp_millis() as f64 * MS_TO_NS;
        span.start = start_time as i64;
        span.meta.extend(HashMap::from([
            ("operation_name".to_string(), "aws.eventbridge".to_string()),
            ("resource_names".to_string(), self.source.clone()),
            ("detail_type".to_string(), self.detail_type.clone()),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        // the only 2 trigger tags seems to be function_trigger.event_source and
        // function_trigger.event_source_arn and they are added in the trigger
        HashMap::new()
    }

    fn get_arn(&self, _region: &str) -> String {
        // TODO not sure what the ARN should be for EventBridge, go-agent is using source
        self.source.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        if let Ok(detail) = serde_json::from_value::<HashMap<String, Value>>(self.detail.clone()) {
            if let Some(datadog) = detail.get("_datadog") {
                if let Ok(datadog_map) =
                    serde_json::from_value::<HashMap<String, String>>(datadog.clone())
                {
                    return datadog_map;
                }
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
        let json = read_json_file("event_bridge.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result =
            EventBridgeEvent::new(payload).expect("Failed to deserialize into EventBridgeEvent");

        let expected = EventBridgeEvent {
            id: "bd3c8258-8d30-007c-2562-64715b2d0ea8".to_string(),
            version: "0".to_string(),
            account: "601427279990".to_string(),
            time: "2022-01-24T16:00:10Z".to_string(),
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
        let json = read_json_file("event_bridge.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize APIGatewayRestEvent");

        assert!(EventBridgeEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize APIGatewayRestEvent");
        assert!(!EventBridgeEvent::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("event_bridge.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            EventBridgeEvent::new(payload).expect("Failed to deserialize into EventBridgeEvent");

        let mut span = Span::default();
        event.enrich_span(&mut span);

        let expected_span = serde_json::from_str(&read_json_file("event_bridge_span.json"))
            .expect("Failed to deserialize into Span");
        assert_eq!(span, expected_span);
    }

    #[test]
    fn test_enrich_parameterized_span() {
        //TODO
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("event_bridge.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let arn =
            EventBridgeEvent::new(payload).expect("Failed to deserialize EventBridgeEvent").get_arn("don't care");
        assert_eq!(arn, "my.event");
    }
}
