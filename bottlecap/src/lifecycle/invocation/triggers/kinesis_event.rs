#![allow(clippy::module_name_repetitions)]
use base64::Engine;
use base64::engine::general_purpose;
use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::{Value, from_slice};
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::S_TO_NS,
    triggers::{
        DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, ServiceNameResolver, Trigger,
    },
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
    pub data: String,
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
        {
            first_record.get("kinesis").is_some()
        } else {
            false
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(
        &self,
        span: &mut Span,
        service_mapping: &HashMap<String, String>,
        aws_service_representation_enabled: bool,
    ) {
        let stream_name = self.get_specific_identifier();
        let shard_id = self.event_id.split(':').next().unwrap_or_default();
        let service_name = self.resolve_service_name(
            service_mapping,
            &stream_name,
            "kinesis",
            aws_service_representation_enabled,
        );

        span.name = String::from("aws.kinesis");
        span.service = service_name;
        span.start = (self.kinesis.approximate_arrival_timestamp * S_TO_NS) as i64;
        span.resource.clone_from(&stream_name);
        span.r#type = "web".to_string();
        span.meta = HashMap::from([
            ("operation_name".to_string(), "aws.kinesis".to_string()),
            ("stream_name".to_string(), stream_name.clone()),
            ("shard_id".to_string(), shard_id.to_string()),
            (
                "event_source_arn".to_string(),
                self.event_source_arn.clone(),
            ),
            ("event_id".to_string(), self.event_id.clone()),
            ("event_name".to_string(), self.event_name.clone()),
            ("event_version".to_string(), self.event_version.clone()),
            (
                "partition_key".to_string(),
                self.kinesis.partition_key.clone(),
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
        if let Ok(decoded_base64) = general_purpose::STANDARD.decode(&self.kinesis.data)
            && let Ok(as_json_map) = from_slice::<HashMap<String, Value>>(&decoded_base64)
            && let Some(carrier) = as_json_map.get(DATADOG_CARRIER_KEY)
        {
            return serde_json::from_value(carrier.clone()).unwrap_or_default();
        }
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }
}

impl ServiceNameResolver for KinesisRecord {
    fn get_specific_identifier(&self) -> String {
        self.event_source_arn
            .split('/')
            .next_back()
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_kinesis"
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
                data: "eyJmb28iOiAiYmFyIiwgIl9kYXRhZG9nIjogeyJ4LWRhdGFkb2ctdHJhY2UtaWQiOiAiNDk0ODM3NzMxNjM1NzI5MTQyMSIsICJ4LWRhdGFkb2ctcGFyZW50LWlkIjogIjI4NzYyNTMzODAwMTg2ODEwMjYiLCAieC1kYXRhZG9nLXNhbXBsaW5nLXByaW9yaXR5IjogIjEifX0=".to_string(),
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
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);
        assert_eq!(span.name, "aws.kinesis");
        assert_eq!(span.service, "kinesisStream");
        assert_eq!(span.resource, "kinesisStream");
        assert_eq!(span.r#type, "web");

        assert_eq!(
            span.meta,
            HashMap::from([
                ("operation_name".to_string(), "aws.kinesis".to_string()),
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

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "4948377316357291421".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "2876253380018681026".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_resolve_service_name_with_representation_enabled() {
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = KinesisRecord::new(payload).expect("Failed to deserialize KinesisRecord");

        // Test 1: Specific mapping takes priority
        let specific_service_mapping = HashMap::from([
            ("kinesisStream".to_string(), "specific-service".to_string()),
            ("lambda_kinesis".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "kinesis",
                true // aws_service_representation_enabled
            ),
            "specific-service"
        );

        // Test 2: Generic mapping is used when specific not found
        let generic_service_mapping =
            HashMap::from([("lambda_kinesis".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "kinesis",
                true // aws_service_representation_enabled
            ),
            "generic-service"
        );

        // Test 3: When no mapping exists, uses instance name (stream name)
        let empty_mapping = HashMap::new();
        assert_eq!(
            event.resolve_service_name(
                &empty_mapping,
                &event.get_specific_identifier(),
                "kinesis",
                true // aws_service_representation_enabled
            ),
            event.get_specific_identifier() // instance name
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_disabled() {
        let json = read_json_file("kinesis_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = KinesisRecord::new(payload).expect("Failed to deserialize KinesisRecord");

        // Test 1: With specific mapping - still respects mapping
        let specific_service_mapping = HashMap::from([
            ("kinesisStream".to_string(), "specific-service".to_string()),
            ("lambda_kinesis".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "kinesis",
                false // aws_service_representation_enabled = false
            ),
            "specific-service"
        );

        // Test 2: With generic mapping - still respects mapping
        let generic_service_mapping =
            HashMap::from([("lambda_kinesis".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "kinesis",
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
                "kinesis",
                false // aws_service_representation_enabled = false
            ),
            "kinesis" // fallback value
        );
    }
}
