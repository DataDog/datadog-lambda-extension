use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{get_aws_partition_by_region, Trigger},
};

use super::DATADOG_CARRIER_KEY;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SqsEvent {
    #[serde(rename = "Records")]
    pub records: Vec<SqsRecord>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct SqsRecord {
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "receiptHandle")]
    pub receipt_handle: String,
    pub body: String,
    pub attributes: Attributes,
    #[serde(rename = "messageAttributes")]
    pub message_attributes: HashMap<String, MessageAttribute>,
    #[serde(rename = "md5OfBody")]
    pub md5_of_body: String,
    #[serde(rename = "eventSource")]
    pub event_source: String,
    #[serde(rename = "eventSourceARN")]
    pub event_source_arn: String,
    #[serde(rename = "awsRegion")]
    pub aws_region: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MessageAttribute {
    #[serde(rename = "stringValue")]
    pub string_value: Option<String>,
    #[serde(rename = "binaryValue")]
    pub binary_value: Option<String>,
    #[serde(rename = "stringListValues")]
    pub string_list_values: Option<Vec<String>>,
    #[serde(rename = "binaryListValues")]
    pub binary_list_values: Option<Vec<String>>,
    #[serde(rename = "dataType")]
    pub data_type: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Attributes {
    #[serde(rename = "ApproximateFirstReceiveTimestamp")]
    pub approximate_first_receive_timestamp: String,
    #[serde(rename = "ApproximateReceiveCount")]
    pub approximate_receive_count: String,
    #[serde(rename = "SentTimestamp")]
    pub sent_timestamp: String,
    #[serde(rename = "SenderId")]
    pub sender_id: String,
}

impl Trigger for SqsRecord {
    fn new(payload: Value) -> Option<Self> {
        let records = payload.get("Records").and_then(Value::as_array);
        match records {
            Some(records) => match serde_json::from_value::<SqsRecord>(records[0].clone()) {
                Ok(event) => Some(event),
                Err(e) => {
                    debug!("Failed to deserialize SQS Record: {e}");
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
            first_record
                .get("eventSource")
                .and_then(Value::as_str)
                .map_or(false, |s| s == "aws:sqs")
        } else {
            false
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span) {
        debug!("Enriching an Inferred Span for an SQS Event");
        let resource = self
            .event_source_arn
            .clone()
            .split(':')
            .last()
            .unwrap_or_default()
            .to_string();
        let start_time = (self
            .attributes
            .sent_timestamp
            .parse::<u64>()
            .unwrap_or_default() as f64
            * MS_TO_NS) as i64;
        // todo: service mapping
        let service_name = "sqs";

        span.name = "aws.sqs".to_string();
        span.service = service_name.to_string();
        span.resource.clone_from(&resource);
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend(HashMap::from([
            ("operation_name".to_string(), "aws.sqs".to_string()),
            ("receipt_handle".to_string(), self.receipt_handle.clone()),
            (
                "retry_count".to_string(),
                self.attributes.approximate_receive_count.clone(),
            ),
            (
                "retry_count".to_string(),
                self.attributes.approximate_receive_count.clone(),
            ),
            ("sender_id".to_string(), self.attributes.sender_id.clone()),
            ("source_arn".to_string(), self.event_source_arn.clone()),
            ("aws_region".to_string(), self.aws_region.clone()),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([
            (
                "retry_count".to_string(),
                self.attributes.approximate_receive_count.clone(),
            ),
            ("sender_id".to_string(), self.attributes.sender_id.clone()),
            ("source_arn".to_string(), self.event_source_arn.clone()),
            ("aws_region".to_string(), self.aws_region.clone()),
        ])
    }

    fn get_arn(&self, region: &str) -> String {
        if let [_, _, _, _, account, queue_name] = self
            .event_source_arn
            .split(':')
            .collect::<Vec<&str>>()
            .as_slice()
        {
            format!(
                "arn:aws:sqs:{}:{}:{}",
                get_aws_partition_by_region(region),
                account,
                queue_name
            )
        } else {
            String::new()
        }
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        let carrier = HashMap::new();
        if let Some(ma) = self.message_attributes.get(DATADOG_CARRIER_KEY) {
            if ma.data_type == "String" {
                if let Some(string_value) = &ma.string_value {
                    return serde_json::from_str(string_value).unwrap_or_default();
                }
            }
        }

        carrier
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = SqsRecord::new(payload).expect("Failed to deserialize into Record");

        let expected = SqsRecord {
            message_id: "19dd0b57-b21e-4ac1-bd88-01bbb068cb78".to_string(),
            receipt_handle: "MessageReceiptHandle".to_string(),
            body: "Hello from SQS!".to_string(),
            attributes: Attributes {
                approximate_first_receive_timestamp: "1523232000001".to_string(),
                approximate_receive_count: "1".to_string(),
                sent_timestamp: "1523232000000".to_string(),
                sender_id: "123456789012".to_string(),
            },
            message_attributes: HashMap::from([]),
            md5_of_body: "{{{md5_of_body}}}".to_string(),
            event_source: "aws:sqs".to_string(),
            event_source_arn: "arn:aws:sqs:us-east-1:123456789012:MyQueue".to_string(),
            aws_region: "us-east-1".to_string(),
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");

        assert!(SqsRecord::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");
        assert!(!SqsRecord::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");
        let mut span = Span::default();
        event.enrich_span(&mut span);
        assert_eq!(span.name, "aws.sqs");
        assert_eq!(span.service, "sqs");
        assert_eq!(span.resource, "MyQueue");
        assert_eq!(span.r#type, "web");

        assert_eq!(
            span.meta,
            HashMap::from([
                ("operation_name".to_string(), "aws.sqs".to_string()),
                (
                    "receipt_handle".to_string(),
                    "MessageReceiptHandle".to_string(),
                ),
                ("retry_count".to_string(), 1.to_string()),
                ("sender_id".to_string(), "123456789012".to_string()),
                (
                    "source_arn".to_string(),
                    "arn:aws:sqs:us-east-1:123456789012:MyQueue".to_string()
                ),
                ("aws_region".to_string(), "us-east-1".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");
        let tags = event.get_tags();

        let expected = HashMap::from([
            ("retry_count".to_string(), 1.to_string()),
            ("sender_id".to_string(), "123456789012".to_string()),
            (
                "source_arn".to_string(),
                "arn:aws:sqs:us-east-1:123456789012:MyQueue".to_string(),
            ),
            ("aws_region".to_string(), "us-east-1".to_string()),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:sqs:us-east-1:123456789012:MyQueue"
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "2684756524522091840".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "7431398482019833808".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }
}
