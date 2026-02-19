use crate::config::aws::get_aws_partition_by_region;
use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{
        DATADOG_CARRIER_KEY, FUNCTION_TRIGGER_EVENT_SOURCE_TAG, ServiceNameResolver, Trigger,
        event_bridge_event::EventBridgeEvent,
        sns_event::{SnsEntity, SnsRecord},
    },
};
use crate::traces::context::{Sampling, SpanContext};
use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

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
    pub body: String,
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
    #[serde(rename = "AWSTraceHeader")]
    pub aws_trace_header: Option<String>,
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
        {
            first_record
                .get("eventSource")
                .and_then(Value::as_str)
                .is_some_and(|s| s == "aws:sqs")
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
        debug!("Enriching an Inferred Span for an SQS Event");
        let resource = self.get_specific_identifier();
        let start_time = (self
            .attributes
            .sent_timestamp
            .parse::<i64>()
            .unwrap_or_default() as f64
            * MS_TO_NS) as i64;

        let service_name = self.resolve_service_name(
            service_mapping,
            &self.get_specific_identifier(),
            "sqs",
            aws_service_representation_enabled,
        );

        span.name = "aws.sqs".to_string();
        span.service.clone_from(&service_name);
        span.resource = resource;
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend(HashMap::from([
            ("operation_name".to_string(), "aws.sqs".to_string()),
            ("receipt_handle".to_string(), self.receipt_handle.clone()),
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
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "sqs".to_string(),
            ),
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
                "arn:{}:sqs:{}:{}:{}",
                get_aws_partition_by_region(region),
                region,
                account,
                queue_name
            )
        } else {
            String::new()
        }
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        let carrier = HashMap::new();

        if let Some(ma) = self.message_attributes.get(DATADOG_CARRIER_KEY)
            && let Some(string_value) = &ma.string_value
        {
            return serde_json::from_str(string_value).unwrap_or_default();
        }

        // Check for SNS event sent through SQS
        if let Ok(sns_entity) = serde_json::from_str::<SnsEntity>(&self.body) {
            let sns_record = SnsRecord {
                sns: sns_entity,
                event_subscription_arn: None,
            };

            return sns_record.get_carrier();
        } else if let Ok(event) = serde_json::from_str::<EventBridgeEvent>(&self.body) {
            return event.get_carrier();
        }

        // TODO: AWSTraceHeader
        carrier
    }

    fn is_async(&self) -> bool {
        true
    }
}

impl ServiceNameResolver for SqsRecord {
    fn get_specific_identifier(&self) -> String {
        self.event_source_arn
            .split(':')
            .next_back()
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_sqs"
    }
}

// extractTraceContextfromAWSTraceHeader extracts trace context from the
// AWSTraceHeader directly. Unlike the other carriers in this file, it should
// not be passed to the tracer.Propagator, instead extracting context directly.
pub(crate) fn extract_trace_context_from_aws_trace_header(
    headers_string: Option<String>,
) -> Option<SpanContext> {
    let value = headers_string?;
    if !value.starts_with("Root=") {
        return None;
    }

    let mut trace_id = String::new();
    let mut parent_id = String::new();
    let mut sampled = String::new();

    for part in value.split(';') {
        if part.starts_with("Root=") {
            trace_id = part[24..].to_string();
        } else if let Some(parent_part) = part.strip_prefix("Parent=") {
            parent_id = parent_part.to_string();
        } else if part.starts_with("Sampled=") && sampled.is_empty() {
            sampled = part[8..].to_string();
        }
        if !trace_id.is_empty() && !parent_id.is_empty() && !sampled.is_empty() {
            break;
        }
    }

    let trace_id = u64::from_str_radix(&trace_id, 16).ok()?;
    let parent_id = u64::from_str_radix(&parent_id, 16).ok()?;

    if trace_id == 0 || parent_id == 0 {
        debug!("awstrace_header contains empty trace or parent ID");
        return None;
    }

    let sampling_priority = i8::from(sampled == "1");

    Some(SpanContext {
        // the context from AWS Header is used by Datadog only and does not contain the upper
        // 64 bits like other 128 w3c compliant trace ids
        trace_id,
        span_id: parent_id,
        sampling: Some(Sampling {
            priority: Some(sampling_priority),
            mechanism: None,
        }),
        origin: None,
        tags: HashMap::new(),
        links: Vec::new(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = SqsRecord::new(payload).expect("Failed to deserialize into Record");

        let message_attributes = HashMap::<String, MessageAttribute>::from([
            ("_datadog".to_string(), MessageAttribute {
                string_value: Some("{\"x-datadog-trace-id\":\"2684756524522091840\",\"x-datadog-parent-id\":\"7431398482019833808\",\"x-datadog-sampling-priority\":\"1\"}".to_string()),
                binary_value: None,
                string_list_values: Some(vec![]),
                binary_list_values: Some(vec![]),
                data_type: "String".to_string(),
            })
        ]);

        let expected = SqsRecord {
            message_id: "19dd0b57-b21e-4ac1-bd88-01bbb068cb78".to_string(),
            receipt_handle: "MessageReceiptHandle".to_string(),
            attributes: Attributes {
                approximate_first_receive_timestamp: "1523232000001".to_string(),
                approximate_receive_count: "1".to_string(),
                sent_timestamp: "1523232000000".to_string(),
                sender_id: "123456789012".to_string(),
                aws_trace_header: None,
            },
            message_attributes,
            md5_of_body: "{{{md5_of_body}}}".to_string(),
            event_source: "aws:sqs".to_string(),
            event_source_arn: "arn:aws:sqs:us-east-1:123456789012:MyQueue".to_string(),
            aws_region: "us-east-1".to_string(),
            body: "Hello from SQS!".to_string(),
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
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);
        assert_eq!(span.name, "aws.sqs");
        assert_eq!(span.service, "MyQueue");
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
            (
                "function_trigger.event_source".to_string(),
                "sqs".to_string(),
            ),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("sqs_event.json");
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

    #[test]
    fn test_get_carrier_from_sns() {
        let json = read_json_file("sns_sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "2776434475358637757".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "4493917105238181843".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_get_carrier_from_sns_binary() {
        let json = read_json_file("sns_sqs_binary_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "5863834085596065348".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "2752725546543693249".to_string(),
            ),
            (
                "tracestate".to_string(),
                "dd=s:1;p:2633a54ccde13dc1;t.tid:6801584a00000000;t.dm:-1".to_string(),
            ),
            (
                "traceparent".to_string(),
                "00-6801584a00000000516086086dc7ee44-2633a54ccde13dc1-01".to_string(),
            ),
            (
                "x-datadog-tags".to_string(),
                "_dd.p.dm=-1,_dd.p.tid=6801584a00000000".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_get_carrier_from_eventbridge() {
        let json = read_json_file("eventbridge_sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize EventBridgeEvent");
        let carrier = event.get_carrier();

        let expected = HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "7379586022458917877".to_string(),
            ),
            (
                "traceparent".to_string(),
                "00-000000000000000066698e63821a03f5-24b17e9b6476c018-01".to_string(),
            ),
            ("x-datadog-tags".to_string(), "_dd.p.dm=-0".to_string()),
            (
                "x-datadog-parent-id".to_string(),
                "2644033662113726488".to_string(),
            ),
            ("tracestate".to_string(), "dd=t.dm:-0;s:1".to_string()),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_resolve_service_name_with_representation_enabled() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");

        // Test 1: Specific mapping takes priority
        let specific_service_mapping = HashMap::from([
            ("MyQueue".to_string(), "specific-service".to_string()),
            ("lambda_sqs".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "sqs",
                true // aws_service_representation_enabled
            ),
            "specific-service"
        );

        // Test 2: Generic mapping is used when specific not found
        let generic_service_mapping =
            HashMap::from([("lambda_sqs".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "sqs",
                true // aws_service_representation_enabled
            ),
            "generic-service"
        );

        // Test 3: When no mapping exists, uses instance name (queue name)
        let empty_mapping = HashMap::new();
        assert_eq!(
            event.resolve_service_name(
                &empty_mapping,
                &event.get_specific_identifier(),
                "sqs",
                true // aws_service_representation_enabled
            ),
            event.get_specific_identifier() // instance name
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_disabled() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize SqsRecord");

        // Test 1: With specific mapping - still respects mapping
        let specific_service_mapping = HashMap::from([
            ("MyQueue".to_string(), "specific-service".to_string()),
            ("lambda_sqs".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.get_specific_identifier(),
                "sqs",
                false // aws_service_representation_enabled = false
            ),
            "specific-service"
        );

        // Test 2: With generic mapping - still respects mapping
        let generic_service_mapping =
            HashMap::from([("lambda_sqs".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.get_specific_identifier(),
                "sqs",
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
                "sqs",
                false // aws_service_representation_enabled = false
            ),
            "sqs" // fallback value
        );
    }

    #[test]
    fn extract_java_sqs_header_context() {
        let json = read_json_file("eventbridge_sqs_java_header_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = SqsRecord::new(payload).expect("Failed to deserialize EventBridgeEvent");

        assert_eq!(
            extract_trace_context_from_aws_trace_header(Some(
                event.attributes.aws_trace_header.unwrap().clone()
            ))
            .unwrap(),
            SpanContext {
                trace_id: 130_944_522_478_755_159,
                span_id: 9_032_698_535_745_367_362,
                sampling: Some(Sampling {
                    priority: Some("0".parse().unwrap()),
                    mechanism: None,
                }),
                origin: None,
                tags: HashMap::new(),
                links: Vec::new(),
            }
        );
    }
}
