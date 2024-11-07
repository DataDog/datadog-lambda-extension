use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{base64_to_string, Trigger, DATADOG_CARRIER_KEY},
};

use super::{FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, FUNCTION_TRIGGER_EVENT_SOURCE_TAG};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnsEvent {
    #[serde(rename = "Records")]
    pub records: Vec<SnsRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnsRecord {
    #[serde(rename = "Sns")]
    pub sns: SnsEntity,
    #[serde(rename = "EventSubscriptionArn")]
    pub event_subscription_arn: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SnsEntity {
    #[serde(rename = "MessageId")]
    pub message_id: String,
    #[serde(rename = "Type")]
    pub r#type: String,
    #[serde(rename = "TopicArn")]
    pub topic_arn: String,
    #[serde(rename = "MessageAttributes")]
    pub message_attributes: HashMap<String, MessageAttribute>,
    #[serde(rename = "Timestamp")]
    pub timestamp: DateTime<Utc>,
    #[serde(rename = "Subject")]
    pub subject: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MessageAttribute {
    #[serde(rename = "Type")]
    pub r#type: String,
    #[serde(rename = "Value")]
    pub value: String,
}

impl Trigger for SnsRecord {
    fn new(payload: serde_json::Value) -> Option<Self> {
        match payload.get("Records").and_then(Value::as_array) {
            Some(records) => match serde_json::from_value::<SnsRecord>(records[0].clone()) {
                Ok(record) => Some(record),
                Err(e) => {
                    debug!("Failed to deserialize SNS Record: {e}");
                    None
                }
            },
            None => None,
        }
    }

    fn is_match(payload: &serde_json::Value) -> bool {
        if let Some(first_record) = payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .take()
        {
            return first_record.get("Sns").is_some();
        }

        false
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut datadog_trace_protobuf::pb::Span) {
        debug!("Enriching an Inferred Span for an SNS Event");
        let resource = self
            .sns
            .topic_arn
            .clone()
            .split(':')
            .last()
            .unwrap_or_default()
            .to_string();

        let start_time = self
            .sns
            .timestamp
            .timestamp_nanos_opt()
            .unwrap_or((self.sns.timestamp.timestamp_millis() as f64 * MS_TO_NS) as i64);
        // todo: service mapping
        let service_name = "sns".to_string();

        span.name = "aws.sns".to_string();
        span.service = service_name.to_string();
        span.resource.clone_from(&resource);
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend([
            ("operation_name".to_string(), "aws.sqs".to_string()),
            ("topicname".to_string(), resource),
            ("topic_arn".to_string(), self.sns.topic_arn.clone()),
            ("message_id".to_string(), self.sns.message_id.clone()),
            ("type".to_string(), self.sns.r#type.clone()),
        ]);

        if let Some(subject) = &self.sns.subject {
            span.meta.insert("subject".to_string(), subject.clone());
        }

        if let Some(event_subscription_arn) = &self.event_subscription_arn {
            span.meta.insert(
                "event_subscription_arn".to_string(),
                event_subscription_arn.clone(),
            );
        }
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "sns".to_string(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                self.sns.topic_arn.clone(),
            ),
        ])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.sns.topic_arn.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        let carrier = HashMap::new();
        if let Some(ma) = self.sns.message_attributes.get(DATADOG_CARRIER_KEY) {
            match ma.r#type.as_str() {
                "String" => return serde_json::from_str(&ma.value).unwrap_or_default(),
                "Binary" => {
                    return serde_json::from_str(&base64_to_string(&ma.value)).unwrap_or_default()
                }
                _ => {
                    debug!("Unsupported type in SNS message attribute");
                }
            }
        }

        carrier
    }

    fn is_async(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_protobuf::pb::Span;

    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("sns_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = SnsRecord::new(payload).expect("Failed to deserialize into SnsRecord");

        let message_attributes = HashMap::<String, MessageAttribute>::from([
            ("_datadog".to_string(), MessageAttribute {
                r#type: "String".to_string(),
                value: "{\"x-datadog-trace-id\": \"4948377316357291421\", \"x-datadog-parent-id\": \"6746998015037429512\", \"x-datadog-sampling-priority\": \"1\"}".to_string(),
            })
        ]);

        let expected = SnsRecord {
            event_subscription_arn: Some("arn:aws:sns:sa-east-1:425362996713:serverlessTracingTopicPy:224b60ba-befc-4830-ad96-f1f0ac94eb04".to_string()),
            sns: SnsEntity {
                message_id: "87056a47-f506-5d77-908b-303605d3b197".to_string(),
                r#type: "Notification".to_string(),
                topic_arn: "arn:aws:sns:sa-east-1:425362996713:serverlessTracingTopicPy"
                    .to_string(),
                message_attributes,
                timestamp: DateTime::parse_from_rfc3339("2022-01-31T14:13:41.637Z")
                    .unwrap()
                    .with_timezone(&Utc),
                subject: None,
            },
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("sns_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SnsRecord");

        assert!(SnsRecord::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");
        assert!(!SnsRecord::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("sns_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SnsRecord::new(payload).expect("Failed to deserialize SnsRecord");
        let mut span = Span::default();
        event.enrich_span(&mut span);
        assert_eq!(span.name, "aws.sns");
        assert_eq!(span.service, "sns");
        assert_eq!(span.resource, "serverlessTracingTopicPy");
        assert_eq!(span.r#type, "web");

        assert_eq!(
            span.meta,
            HashMap::from([
                ("operation_name".to_string(), "aws.sns".to_string()),
                ("topicname".to_string(), "serverlessTracingTopicPy".to_string()),
                ("topic_arn".to_string(), "arn:aws:sns:sa-east-1:425362996713:serverlessTracingTopicPy".to_string()),
                ("message_id".to_string(), "87056a47-f506-5d77-908b-303605d3b197".to_string()),
                ("type".to_string(), "Notification".to_string()),
                ("event_subscription_arn".to_string(), "arn:aws:sns:sa-east-1:425362996713:serverlessTracingTopicPy:224b60ba-befc-4830-ad96-f1f0ac94eb04".to_string())
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("sns_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SnsRecord::new(payload).expect("Failed to deserialize SnsRecord");
        let tags = event.get_tags();

        let expected = HashMap::from([
            ("function_trigger.event_source".to_string(), "sns".to_string()),
            (
                "function_trigger.event_source_arn".to_string(),
                "arn:aws:sns:sa-east-1:425362996713:serverlessTracingTopicPy".to_string(),
            ),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("sns_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SnsRecord::new(payload).expect("Failed to deserialize SnsRecord");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:sns:sa-east-1:425362996713:serverlessTracingTopicPy"
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("sns_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SnsRecord::new(payload).expect("Failed to deserialize SnsRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "4948377316357291421".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "6746998015037429512".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_get_carrier_from_binary_value() {
        let json = read_json_file("sns_event_binary.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SnsRecord::new(payload).expect("Failed to deserialize SnsRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "4948377316357291421".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "6746998015037429512".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }
}
