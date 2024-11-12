use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::S_TO_NS,
    triggers::{Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG},
};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbEvent {
    #[serde(rename = "Records")]
    pub records: Vec<DynamoDbRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbRecord {
    #[serde(rename = "dynamodb")]
    pub dynamodb: DynamoDbEntity,
    #[serde(rename = "eventID")]
    pub event_id: String,
    #[serde(rename = "eventName")]
    pub event_name: String,
    #[serde(rename = "eventVersion")]
    pub event_version: String,
    #[serde(rename = "eventSourceARN")]
    pub event_source_arn: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbEntity {
    #[serde(rename = "ApproximateCreationDateTime")]
    pub approximate_creation_date_time: f64,
    #[serde(rename = "SizeBytes")]
    pub size_bytes: i64,
    #[serde(rename = "StreamViewType")]
    pub stream_view_type: String,
}

impl Trigger for DynamoDbRecord {
    fn new(payload: Value) -> Option<Self>
    where
        Self: Sized,
    {
        let records = payload.get("Records").and_then(Value::as_array);
        match records {
            Some(records) => match serde_json::from_value::<DynamoDbRecord>(records[0].clone()) {
                Ok(event) => Some(event),
                Err(e) => {
                    debug!("Failed to deserialize DynamoDB Record: {e}");
                    None
                }
            },
            None => None,
        }
    }

    fn is_match(payload: &Value) -> bool
    where
        Self: Sized,
    {
        if let Some(first_record) = payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .take()
        {
            first_record.get("dynamodb").is_some()
        } else {
            false
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span) {
        debug!("Enriching an Inferred Span for a DynamoDB event");
        let table_name = self.event_source_arn.split('/').nth(1).unwrap_or_default();
        let resource = format!("{} {}", self.event_name.clone(), table_name);

        let start_time = (self.dynamodb.approximate_creation_date_time * S_TO_NS) as i64;
        // todo: service mapping and peer service
        let service_name = "dynamodb";

        span.name = String::from("aws.dynamodb");
        span.service = service_name.to_string();
        span.resource = resource;
        span.r#type = String::from("web");
        span.start = start_time;
        span.meta.extend(HashMap::from([
            ("operation_name".to_string(), String::from("aws.dynamodb")),
            ("event_id".to_string(), self.event_id.clone()),
            ("event_name".to_string(), self.event_name.clone()),
            ("event_version".to_string(), self.event_version.clone()),
            (
                "event_source_arn".to_string(),
                self.event_source_arn.clone(),
            ),
            (
                "size_bytes".to_string(),
                self.dynamodb.size_bytes.to_string(),
            ),
            (
                "stream_view_type".to_string(),
                self.dynamodb.stream_view_type.clone(),
            ),
            ("table_name".to_string(), table_name.to_string()),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "dynamodb".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.event_source_arn.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
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
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = DynamoDbRecord::new(payload).expect("Failed to deserialize into Record");

        let expected = DynamoDbRecord {
            dynamodb: DynamoDbEntity {
                approximate_creation_date_time: 1_428_537_600.0,
                size_bytes: 26,
                stream_view_type: String::from("NEW_AND_OLD_IMAGES"),
            },
            event_id: String::from("c4ca4238a0b923820dcc509a6f75849b"),
            event_name: String::from("INSERT"),
            event_version: String::from("1.1"),
            event_source_arn: String::from("arn:aws:dynamodb:us-east-1:123456789012:table/ExampleTableWithStream/stream/2015-06-27T00:48:05.899"),
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize DynamoDbRecord");

        assert!(DynamoDbRecord::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");
        assert!(!DynamoDbRecord::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        let mut span = Span::default();
        event.enrich_span(&mut span);
        assert_eq!(span.name, "aws.dynamodb");
        assert_eq!(span.service, "dynamodb");
        assert_eq!(span.resource, "INSERT ExampleTableWithStream");
        assert_eq!(span.r#type, "web");

        assert_eq!(
            span.meta,
            HashMap::from([
                ("operation_name".to_string(), "aws.dynamodb".to_string()),
                ("event_id".to_string(), "c4ca4238a0b923820dcc509a6f75849b".to_string()),
                ("event_name".to_string(), "INSERT".to_string()),
                ("event_version".to_string(), "1.1".to_string()),
                (
                    "event_source_arn".to_string(),
                    "arn:aws:dynamodb:us-east-1:123456789012:table/ExampleTableWithStream/stream/2015-06-27T00:48:05.899".to_string()
                ),
                ("size_bytes".to_string(), "26".to_string()),
                ("stream_view_type".to_string(), "NEW_AND_OLD_IMAGES".to_string()),
                ("table_name".to_string(), "ExampleTableWithStream".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        let tags = event.get_tags();

        let expected = HashMap::from([(
            "function_trigger.event_source".to_string(),
            "dynamodb".to_string(),
        )]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:dynamodb:us-east-1:123456789012:table/ExampleTableWithStream/stream/2015-06-27T00:48:05.899"
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::new();

        assert_eq!(carrier, expected);
    }
}
