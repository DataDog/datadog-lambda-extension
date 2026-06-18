use chrono::{DateTime, Utc};
use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::{MS_TO_NS, S_TO_NS},
    triggers::{
        DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, ServiceNameResolver, Trigger,
    },
};

const DATADOG_START_TIME_KEY: &str = "x-datadog-start-time";
const DATADOG_RESOURCE_NAME_KEY: &str = "x-datadog-resource-name";

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
                .is_some_and(|s| s != "aws.events")
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(
        &self,
        span: &mut Span,
        service_mapping: &HashMap<String, String>,
        aws_service_representation_enabled: bool,
    ) {
        // EventBridge events have a timestamp resolution in seconds
        let start_time_seconds = self
            .time
            .timestamp_nanos_opt()
            .unwrap_or((self.time.timestamp_millis() as f64 * S_TO_NS) as i64);

        let carrier = self.get_carrier();
        let resource_name = self.get_specific_identifier();
        let start_time = carrier
            .get(DATADOG_START_TIME_KEY)
            .and_then(|s| s.parse::<f64>().ok())
            .map_or(start_time_seconds, |s| (s * MS_TO_NS) as i64);

        let service_name = self.resolve_service_name(
            service_mapping,
            &self.get_specific_identifier(),
            "eventbridge",
            aws_service_representation_enabled,
        );

        span.name = String::from("aws.eventbridge");
        span.service.clone_from(&service_name);
        span.resource = resource_name;
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
        if let Ok(detail) = serde_json::from_value::<HashMap<String, Value>>(self.detail.clone())
            && let Some(carrier) = detail.get(DATADOG_CARRIER_KEY)
        {
            return serde_json::from_value(carrier.clone()).unwrap_or_default();
        }
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_dsm_edge_tags(&self) -> Option<Vec<String>> {
        // EventBridge consume edge tags. `topic` is the detail-type. `exchange`
        // is the event bus name, which is NOT carried in the event delivered to
        // Lambda; when a triggering rule ARN is present in `resources` it is
        // encoded as `:rule/<bus>/<rule>`, so we recover it best-effort and omit
        // the tag entirely when it cannot be determined (rather than emit a
        // wrong/empty value that would corrupt the pathway hash).
        let mut tags = vec!["direction:in".to_string(), "type:eventbridge".to_string()];
        if let Some(bus) = self.event_bus_name() {
            tags.push(format!("exchange:{bus}"));
        }
        tags.push(format!("topic:{}", self.detail_type));
        Some(tags)
    }
}

impl EventBridgeEvent {
    /// Best-effort event bus name recovered from a triggering rule ARN in
    /// `resources`. Non-default buses encode the name as `:rule/<bus>/<rule>`;
    /// the default bus appears as `:rule/<rule>` (no bus segment). Returns
    /// `None` when no bus name can be determined from the payload.
    fn event_bus_name(&self) -> Option<String> {
        for arn in &self.resources {
            if let Some(rest) = arn.split(":rule/").nth(1) {
                let mut segments = rest.split('/');
                let first = segments.next().unwrap_or_default();
                // `rule/<bus>/<rule>` => bus is the first segment.
                // `rule/<rule>` (default bus) => no second segment, skip.
                if segments.next().is_some() && !first.is_empty() {
                    return Some(first.to_string());
                }
            }
        }
        None
    }
}

impl ServiceNameResolver for EventBridgeEvent {
    fn get_specific_identifier(&self) -> String {
        let carrier = self.get_carrier();
        carrier
            .get(DATADOG_RESOURCE_NAME_KEY)
            .unwrap_or(&self.source)
            .clone()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_eventbridge"
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
            time: DateTime::parse_from_rfc3339("2024-11-09T08:22:15Z")
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
                    "x-datadog-sampling-priority": "1",
                    "x-datadog-resource-name": "testBus",
                    "x-datadog-start-time": "1731183820135"
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
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);

        let expected = serde_json::from_str(&read_json_file("eventbridge_span.json"))
            .expect("Failed to deserialize into Span");
        assert_eq!(span, expected);
    }

    #[test]
    fn test_enrich_span_no_resource_name() {
        let json = read_json_file("eventbridge_no_resource_name_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            EventBridgeEvent::new(payload).expect("Failed to deserialize into EventBridgeEvent");

        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);

        assert_eq!(span.resource, "my.event");
    }

    #[test]
    fn test_enrich_span_no_timestamp() {
        let json = read_json_file("eventbridge_no_timestamp_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            EventBridgeEvent::new(payload).expect("Failed to deserialize into EventBridgeEvent");

        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);

        assert_eq!(span.resource, "testBus");
        // Seconds resolution
        assert_eq!(span.start, 1_731_140_535_000_000_000);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = EventBridgeEvent::new(payload).expect("Failed to deserialize EventBridgeEvent");
        assert_eq!(event.get_arn("us-east-1"), "my.event");
    }

    fn make_event(detail_type: &str, resources: Vec<String>) -> EventBridgeEvent {
        EventBridgeEvent {
            id: "id".to_string(),
            version: "0".to_string(),
            account: "123456789012".to_string(),
            time: Utc::now(),
            region: "us-east-1".to_string(),
            resources,
            source: "my.event".to_string(),
            detail_type: detail_type.to_string(),
            detail: serde_json::json!({}),
            replay_name: None,
        }
    }

    #[test]
    fn test_get_dsm_edge_tags_no_resources_omits_exchange() {
        // The standard fixture has no `resources`, so the bus name is unknown
        // and the exchange tag must be omitted.
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            EventBridgeEvent::new(payload).expect("Failed to deserialize EventBridge Event");
        assert_eq!(
            event.get_dsm_edge_tags(),
            Some(vec![
                "direction:in".to_string(),
                "type:eventbridge".to_string(),
                "topic:UserSignUp".to_string(),
            ])
        );
    }

    #[test]
    fn test_get_dsm_edge_tags_recovers_bus_from_rule_arn() {
        let event = make_event(
            "OrderPlaced",
            vec!["arn:aws:events:us-east-1:123456789012:rule/my-bus/my-rule".to_string()],
        );
        assert_eq!(
            event.get_dsm_edge_tags(),
            Some(vec![
                "direction:in".to_string(),
                "type:eventbridge".to_string(),
                "exchange:my-bus".to_string(),
                "topic:OrderPlaced".to_string(),
            ])
        );
    }

    #[test]
    fn test_get_dsm_edge_tags_default_bus_rule_arn_omits_exchange() {
        // Default-bus rule ARNs have no bus segment (`:rule/<rule>`).
        let event = make_event(
            "OrderPlaced",
            vec!["arn:aws:events:us-east-1:123456789012:rule/my-rule".to_string()],
        );
        assert_eq!(
            event.get_dsm_edge_tags(),
            Some(vec![
                "direction:in".to_string(),
                "type:eventbridge".to_string(),
                "topic:OrderPlaced".to_string(),
            ])
        );
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
            ("x-datadog-resource-name".to_string(), "testBus".to_string()),
            (
                "x-datadog-start-time".to_string(),
                "1731183820135".to_string(),
            ),
        ]);

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_resolve_service_name_with_representation_enabled() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = EventBridgeEvent::new(payload).expect("Failed to deserialize EventBridgeEvent");

        // Test 1: Specific mapping takes priority
        let specific_service_mapping = HashMap::from([
            ("testBus".to_string(), "specific-service".to_string()),
            (
                "lambda_eventbridge".to_string(),
                "generic-service".to_string(),
            ),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "eventbridge",
                true // aws_service_representation_enabled
            ),
            "specific-service"
        );

        // Test 2: Generic mapping is used when specific not found
        let generic_service_mapping = HashMap::from([(
            "lambda_eventbridge".to_string(),
            "generic-service".to_string(),
        )]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "eventbridge",
                true // aws_service_representation_enabled
            ),
            "generic-service"
        );

        // Test 3: When no mapping exists, uses instance name
        let empty_mapping = HashMap::new();
        assert_eq!(
            event.resolve_service_name(
                &empty_mapping,
                &event.get_specific_identifier(),
                "eventbridge",
                true // aws_service_representation_enabled
            ),
            event.get_specific_identifier() // instance name
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_disabled() {
        let json = read_json_file("eventbridge_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = EventBridgeEvent::new(payload).expect("Failed to deserialize EventBridgeEvent");

        // Test 1: With specific mapping - still respects mapping
        let specific_service_mapping = HashMap::from([
            ("testBus".to_string(), "specific-service".to_string()),
            (
                "lambda_eventbridge".to_string(),
                "generic-service".to_string(),
            ),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "eventbridge",
                false // aws_service_representation_enabled = false
            ),
            "specific-service"
        );

        // Test 2: With generic mapping - still respects mapping
        let generic_service_mapping = HashMap::from([(
            "lambda_eventbridge".to_string(),
            "generic-service".to_string(),
        )]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "eventbridge",
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
                "eventbridge",
                false // aws_service_representation_enabled = false
            ),
            "eventbridge" // fallback value
        );
    }
}
