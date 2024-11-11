#![allow(clippy::module_name_repetitions)]
use std::collections::HashMap;

use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::S_TO_NS,
    triggers::{Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG},
};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct KinesisEvent {
    #[serde(rename = "Records")]
    pub records: Vec<KinesisRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct KinesisRecord {
    #[serde(rename = "eventID")]
    pub event_id: String,
    #[serde(rename = "eventName")]
    pub event_name: String,
    #[serde(rename = "eventSourceARN")]
    pub event_source_arn: String,
    #[serde(rename = "eventVersion")]
    pub event_version: String,
    pub kinesis: KinesisEntity,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct KinesisEntity {
    #[serde(rename = "approximateArrivalTimestamp")]
    pub approximate_arrival_timestamp: f64,
    #[serde(rename = "partitionKey")]
    pub partition_key: String,
}

impl Trigger for KinesisRecord {
    fn new(payload: Value) -> Option<Self> {
        let records = payload.get("Records").and_then(Value::as_array);
        match records {
            Some(records) => match serde_json::from_value::<KinesisRecord>(records[0].clone()) {
                Ok(event) => Some(event),
                Err(e) => {
                    debug!("Failed to deserialize Kinesis Record: {e}");
                    None
                }
            },
            None => None,
        }
    }

    fn is_match(payload: &Value) -> bool {
        if let Some(first_record) = payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .take()
        {
            first_record.get("kinesis").is_some()
        } else {
            false
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span) {
        let event_source_arn = &self.event_source_arn;
        let split_source_arn: Vec<&str> = event_source_arn.split('/').collect();
        let parsed_stream_name = split_source_arn.last().unwrap_or(&"");
        let parsed_shard_id = self.event_id.split(':').next().unwrap_or_default();
        span.name = "aws.kinesis".to_string();
        span.service = "kinesis".to_string();
        span.start = (self.kinesis.approximate_arrival_timestamp * S_TO_NS) as i64;
        span.resource = parsed_stream_name.to_string();
        span.r#type = "web".to_string();
        span.meta = HashMap::from([
            ("operation_name".to_string(), "aws.kinesis".to_string()),
            (
                "resource_names".to_string(),
                (*parsed_stream_name).to_string(),
            ),
            ("stream_name".to_string(), (*parsed_stream_name).to_string()),
            ("shard_id".to_string(), parsed_shard_id.to_string()),
            ("event_source_arn".to_string(), event_source_arn.to_string()),
            ("event_id".to_string(), self.event_id.to_string()),
            ("event_name".to_string(), self.event_name.to_string()),
            ("event_version".to_string(), self.event_version.to_string()),
            (
                "partition_key".to_string(),
                self.kinesis.partition_key.to_string(),
            ),
        ]);
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "kinesis".to_string(),
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
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = KinesisRecord::new(payload).expect("Failed to deserialize into Record");

        let expected = KinesisRecord {
            event_id:
                "shardId-000000000002:49624230154685806402418173680709770494154422022871973922"
                    .to_string(),
            event_name: "aws:kinesis:record".to_string(),
            event_source_arn: "arn:aws:kinesis:sa-east-1:425362996713:stream/kinesisStream"
                .to_string(),
            event_version: "1.0".to_string(),
            kinesis: KinesisEntity {
                approximate_arrival_timestamp: 1_643_638_425.163,
                partition_key: "partitionkey".to_string(),
            },
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize S3Record");

        assert!(KinesisRecord::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");
        assert!(!KinesisRecord::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = KinesisRecord::new(payload).expect("Failed to deserialize S3Record");
        let mut span = Span::default();
        event.enrich_span(&mut span);
        assert_eq!(span.name, "aws.kinesis");
        assert_eq!(span.service, "kinesis");
        assert_eq!(span.resource, "kinesisStream");
        assert_eq!(span.r#type, "web");

        assert_eq!(
            span.meta,
            HashMap::from([
                ("operation_name".to_string(), "aws.kinesis".to_string()),
                ("resource_names".to_string(), "kinesisStream".to_string()),
                ("stream_name".to_string(), "kinesisStream".to_string()),
                ("shard_id".to_string(), "shardId-000000000002".to_string()),
                (
                    "event_source_arn".to_string(),
                    "arn:aws:kinesis:sa-east-1:425362996713:stream/kinesisStream".to_string()
                ),
                (
                    "event_id".to_string(),
                    "shardId-000000000002:49624230154685806402418173680709770494154422022871973922"
                        .to_string()
                ),
                ("event_name".to_string(), "aws:kinesis:record".to_string()),
                ("event_version".to_string(), "1.0".to_string()),
                ("partition_key".to_string(), "partitionkey".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = KinesisRecord::new(payload).expect("Failed to deserialize KinesisRecord");
        let tags = event.get_tags();

        let expected = HashMap::from([(
            "function_trigger.event_source".to_string(),
            "kinesis".to_string(),
        )]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = KinesisRecord::new(payload).expect("Failed to deserialize KinesisRecord");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:kinesis:sa-east-1:425362996713:stream/kinesisStream".to_string()
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = KinesisRecord::new(payload).expect("Failed to deserialize KinesisRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::new();

        assert_eq!(carrier, expected);
    }
}
