use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{get_aws_partition_by_region, Trigger},
};

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
        // duration is current_time_epoch - start_time
        span.duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
            - start_time;
        span.meta.extend(HashMap::from([
            ("operation_name".to_string(), "aws.sqs".to_string()),
            ("receipt_handle".to_string(), self.receipt_handle.clone()),
            (
                "retry_count".to_string(),
                self.attributes.approximate_receive_count.clone(),
            ),
            ("sender_id".to_string(), self.event_source.clone()),
            ("source_arn".to_string(), self.event_source_arn.clone()),
            ("aws_region".to_string(), self.aws_region.clone()),
            ("resource_names".to_string(), resource.clone()),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        let tags = HashMap::from([
            (
                "retry_count".to_string(),
                self.attributes.approximate_receive_count.clone(),
            ),
            ("sender_id".to_string(), self.event_source.clone()),
            ("source_arn".to_string(), self.event_source_arn.clone()),
            ("aws_region".to_string(), self.aws_region.clone()),
        ]);

        tags
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
            "".to_string()
        }
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        todo!()
    }
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//     use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

//     #[test]
//     fn test_new() {
//         let json = read_json_file("api_gateway_rest_event.json");
//         let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
//         let result = APIGatewayRestEvent::new(payload)
//             .expect("Failed to deserialize into APIGatewayRestEvent");

//         let expected = APIGatewayRestEvent {
//             headers: HashMap::from([
//                 ("Header1".to_string(), "value1".to_string()),
//                 ("Header2".to_string(), "value2".to_string()),
//             ]),
//             request_context: RequestContext {
//                 stage: "$default".to_string(),
//                 request_id: "id=".to_string(),
//                 api_id: "id".to_string(),
//                 domain_name: "id.execute-api.us-east-1.amazonaws.com".to_string(),
//                 time_epoch: 1_583_349_317_135,
//                 method: "GET".to_string(),
//                 path: "/my/path".to_string(),
//                 protocol: "HTTP/1.1".to_string(),
//                 resource_path: "/path".to_string(),
//                 identity: Identity {
//                     source_ip: "IP".to_string(),
//                     user_agent: "user-agent".to_string(),
//                 },
//             },
//         };

//         assert_eq!(result, expected);
//     }

//     #[test]
//     fn test_is_match() {
//         let json = read_json_file("api_gateway_rest_event.json");
//         let payload =
//             serde_json::from_str(&json).expect("Failed to deserialize APIGatewayRestEvent");

//         assert!(APIGatewayRestEvent::is_match(&payload));
//     }

//     #[test]
//     fn test_is_not_match() {
//         let json = read_json_file("api_gateway_http_event.json");
//         let payload =
//             serde_json::from_str(&json).expect("Failed to deserialize APIGatewayRestEvent");
//         assert!(!APIGatewayRestEvent::is_match(&payload));
//     }

//     #[test]
//     fn test_enrich_span() {
//         let json = read_json_file("api_gateway_rest_event.json");
//         let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
//         let event =
//             APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
//         let mut span = Span::default();
//         event.enrich_span(&mut span);
//         assert_eq!(span.name, "aws.apigateway");
//         assert_eq!(span.service, "id.execute-api.us-east-1.amazonaws.com");
//         assert_eq!(span.resource, "GET /path");
//         assert_eq!(span.r#type, "http");

//         assert_eq!(
//             span.meta,
//             HashMap::from([
//                 ("endpoint".to_string(), "/my/path".to_string()),
//                 (
//                     "http.url".to_string(),
//                     "https://id.execute-api.us-east-1.amazonaws.com/my/path".to_string()
//                 ),
//                 ("http.method".to_string(), "GET".to_string()),
//                 ("http.protocol".to_string(), "HTTP/1.1".to_string()),
//                 ("http.source_ip".to_string(), "IP".to_string()),
//                 ("http.user_agent".to_string(), "user-agent".to_string()),
//                 ("http.route".to_string(), "/path".to_string()),
//                 ("operation_name".to_string(), "aws.apigateway".to_string()),
//                 ("request_id".to_string(), "id=".to_string()),
//                 ("resource_names".to_string(), "GET /path".to_string()),
//             ])
//         );
//     }

//     #[test]
//     fn test_get_tags() {
//         let json = read_json_file("api_gateway_rest_event.json");
//         let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
//         let event =
//             APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
//         let tags = event.get_tags();

//         let expected = HashMap::from([
//             (
//                 "http.url".to_string(),
//                 "https://id.execute-api.us-east-1.amazonaws.com/my/path".to_string(),
//             ),
//             ("http.url_details.path".to_string(), "/my/path".to_string()),
//             ("http.method".to_string(), "GET".to_string()),
//             ("http.route".to_string(), "/path".to_string()),
//             ("http.user_agent".to_string(), "user-agent".to_string()),
//         ]);

//         assert_eq!(tags, expected);
//     }

//     #[test]
//     fn test_enrich_parameterized_span() {
//         let json = read_json_file("api_gateway_rest_event_parameterized.json");
//         let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
//         let event =
//             APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
//         let mut span = Span::default();
//         event.enrich_span(&mut span);
//         assert_eq!(span.name, "aws.apigateway");
//         assert_eq!(
//             span.service,
//             "mcwkra0ya4.execute-api.sa-east-1.amazonaws.com"
//         );
//         assert_eq!(span.resource, "GET /user/{id}");
//         assert_eq!(span.r#type, "http");
//         let expected = HashMap::from([
//             ("endpoint".to_string(), "/dev/user/42".to_string()),
//             (
//                 "http.url".to_string(),
//                 "https://mcwkra0ya4.execute-api.sa-east-1.amazonaws.com/dev/user/42".to_string(),
//             ),
//             ("http.method".to_string(), "GET".to_string()),
//             ("http.protocol".to_string(), "HTTP/1.1".to_string()),
//             ("http.source_ip".to_string(), "76.115.124.192".to_string()),
//             ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
//             ("http.route".to_string(), "/user/{id}".to_string()),
//             ("operation_name".to_string(), "aws.apigateway".to_string()),
//             (
//                 "request_id".to_string(),
//                 "e16399f7-e984-463a-9931-745ba021a27f".to_string(),
//             ),
//             ("resource_names".to_string(), "GET /user/{id}".to_string()),
//         ]);
//         assert_eq!(span.meta, expected);
//     }

//     #[test]
//     fn test_get_tags_parameterized() {
//         let json = read_json_file("api_gateway_rest_event_parameterized.json");
//         let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
//         let event =
//             APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
//         let tags = event.get_tags();

//         assert_eq!(
//             tags,
//             HashMap::from([
//                 (
//                     "http.url".to_string(),
//                     "https://mcwkra0ya4.execute-api.sa-east-1.amazonaws.com/dev/user/42"
//                         .to_string(),
//                 ),
//                 (
//                     "http.url_details.path".to_string(),
//                     "/dev/user/42".to_string(),
//                 ),
//                 ("http.method".to_string(), "GET".to_string()),
//                 ("http.route".to_string(), "/user/{id}".to_string()),
//                 ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
//             ])
//         );
//     }

//     #[test]
//     fn test_get_arn() {
//         let json = read_json_file("api_gateway_rest_event.json");
//         let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
//         let event =
//             APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
//         assert_eq!(
//             event.get_arn("us-east-1"),
//             "arn:aws:apigateway:us-east-1::/restapis/id/stages/$default"
//         );
//     }
// }
