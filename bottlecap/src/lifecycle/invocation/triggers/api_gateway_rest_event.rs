use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{get_aws_partition_by_region, lowercase_key, Trigger},
};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct APIGatewayRestEvent {
    #[serde(serialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    #[serde(rename = "requestContext")]
    pub request_context: RequestContext,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct RequestContext {
    pub stage: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "apiId")]
    pub api_id: String,
    #[serde(rename = "domainName")]
    pub domain_name: String,
    #[serde(rename = "requestTimeEpoch")]
    pub time_epoch: i64,
    #[serde(rename = "httpMethod")]
    pub method: String,
    #[serde(rename = "resourcePath")]
    pub resource_path: String,
    pub path: String,
    pub protocol: String,
    pub identity: Identity,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Identity {
    #[serde(rename = "sourceIp")]
    pub source_ip: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

impl Trigger for APIGatewayRestEvent {
    fn new(payload: Value) -> Option<Self> {
        match serde_json::from_value(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize APIGatewayRestEvent: {}", e);
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        let stage = payload.get("requestContext").and_then(|v| v.get("stage"));
        let http_method = payload.get("httpMethod");
        let resource = payload.get("resource");
        stage.is_some() && http_method.is_some() && resource.is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span) {
        debug!("Enriching an Inferred Span for an API Gateway REST Event");
        let resource = format!(
            "{http_method} {path}",
            http_method = self.request_context.method,
            path = self.request_context.resource_path
        );
        let http_url = format!(
            "https://{domain_name}{path}",
            domain_name = self.request_context.domain_name,
            path = self.request_context.path
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;
        // todo: service mapping
        let service_name = self.request_context.domain_name.clone();

        span.name = "aws.apigateway".to_string();
        span.service = service_name;
        span.resource.clone_from(&resource);
        span.r#type = "http".to_string();
        span.start = start_time;
        span.meta.extend(HashMap::from([
            (
                "endpoint".to_string(),
                self.request_context.path.clone(),
            ),
            ("http.url".to_string(), http_url),
            (
                "http.method".to_string(),
                self.request_context.method.clone(),
            ),
            (
                "http.protocol".to_string(),
                self.request_context.protocol.clone(),
            ),
            (
                "http.source_ip".to_string(),
                self.request_context.identity.source_ip.clone(),
            ),
            (
                "http.user_agent".to_string(),
                self.request_context.identity.user_agent.clone(),
            ),
            ("operation_name".to_string(), "aws.apigateway".to_string()),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
            ("resource_names".to_string(), resource),
        ]));

        debug!("Enriched Span: {:?}", span);
        // todo: update global(? IsAsync if event payload is `Event`
    }

    fn get_tags(&self) -> HashMap<String, String> {
        let mut tags = HashMap::from([
            (
                "http.url".to_string(),
                self.request_context.domain_name.clone(),
            ),
            (
                "http_url_details.path".to_string(),
                self.request_context.path.clone(),
            ),
            (
                "http.method".to_string(),
                self.request_context.method.clone(),
            ),
        ]);

        if let Some(referer) = self.headers.get("referer") {
            tags.insert("http.referer".to_string(), referer.to_string());
        }

        if let Some(user_agent) = self.headers.get("user-agent") {
            tags.insert("http.user_agent".to_string(), user_agent.to_string());
        }

        tags
    }

    fn get_arn(&self, region: &str) -> String {
        let partition = get_aws_partition_by_region(region);
        format!(
            "arn:{partition}:apigateway:{region}::/restapis/{api_id}/stages/{stage}",
            partition = partition,
            region = region,
            api_id = self.request_context.api_id,
            stage = self.request_context.stage
        )
    }

    fn is_async(&self) -> bool {
        self.headers
            .get("x-amz-invocation-type")
            .is_some_and(|v| v == "Event")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = APIGatewayRestEvent::new(payload)
            .expect("Failed to deserialize into APIGatewayRestEvent");

        let expected = APIGatewayRestEvent {
            headers: HashMap::from([
                ("Header1".to_string(), "value1".to_string()),
                ("Header2".to_string(), "value2".to_string()),
            ]),
            request_context: RequestContext {
                stage: "$default".to_string(),
                request_id: "id=".to_string(),
                api_id: "id".to_string(),
                domain_name: "id.execute-api.us-east-1.amazonaws.com".to_string(),
                time_epoch: 1583349317135,
                method: "GET".to_string(),
                path: "/my/path".to_string(),
                protocol: "HTTP/1.1".to_string(),
                resource_path: "/path".to_string(),
                identity: Identity {
                    source_ip: "IP".to_string(),
                    user_agent: "user-agent".to_string(),
                },
            },
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize APIGatewayRestEvent");

        assert!(APIGatewayRestEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize APIGatewayRestEvent");
        assert!(!APIGatewayRestEvent::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        let mut span = Span::default();
        event.enrich_span(&mut span);
        assert_eq!(span.name, "aws.apigateway");
        assert_eq!(
            span.service,
            "id.execute-api.us-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /path");
        assert_eq!(span.r#type, "http");
        let sorted_span_meta = span.meta.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();
        let expected = HashMap::from([
                ("endpoint".to_string(), "/my/path".to_string()),
                (
                    "http.url".to_string(),
                    "https://id.execute-api.us-east-1.amazonaws.com".to_string()
                ),
                ("http.method".to_string(), "GET".to_string()),
                ("http.protocol".to_string(), "HTTP/1.1".to_string()),
                ("http.source_ip".to_string(), "IP".to_string()),
                ("http.user_agent".to_string(), "user-agent".to_string()),
                ("operation_name".to_string(), "aws.api_gateway".to_string()),
                ("request_id".to_string(), "id=".to_string()),
                ("resource_names".to_string(), "GET /path".to_string()),
            ]);
        let sorted_expected = expected.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();
        assert_eq!(
            sorted_span_meta,
            sorted_expected
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        let tags = event.get_tags();
        let sorted_tags_array = tags
            .iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();

        let expected = HashMap::from([
            (
                "http.url".to_string(),
                "x02yirxc7a.execute-api.sa-east-1.amazonaws.com".to_string(),
            ),
            (
                "http_url_details.path".to_string(),
                "/httpapi/get".to_string(),
            ),
            ("http.method".to_string(), "GET".to_string()),
            ("http.route".to_string(), "GET /httpapi/get".to_string()),
            ("http.user_agent".to_string(), "curl/7.64.1".to_string()),
            ("http.referer".to_string(), "".to_string()),
        ]);
        let expected_sorted_array = expected
            .iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();

        assert_eq!(sorted_tags_array, expected_sorted_array);
    }

    #[test]
    fn test_enrich_parameterized_span() {
        let json = read_json_file("api_gateway_rest_event_parameterized.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        let mut span = Span::default();
        event.enrich_span(&mut span);
        assert_eq!(span.name, "aws.apigateway");
        assert_eq!(
            span.service,
            "mcwkra0ya4.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /user/{id}");
        assert_eq!(span.r#type, "http");
        let sorted_span_meta = span.meta.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();
        let expected = HashMap::from([
                ("endpoint".to_string(), "/dev/user/42".to_string()),
                (
                    "http.url".to_string(),
                    "mcwkra0ya4.execute-api.sa-east-1.amazonaws.com".to_string()
                ),
                ("http.method".to_string(), "GET".to_string()),
                ("http.protocol".to_string(), "HTTP/1.1".to_string()),
                ("http.source_ip".to_string(), "76.115.124.192".to_string()),
                ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
                ("operation_name".to_string(), "aws.api_gateway".to_string()),
                ("request_id".to_string(), "mcwkra0ya4".to_string()),
                ("resource_names".to_string(), "GET /dev/user/{id}".to_string()),
            ]);
        let sorted_expected = expected.iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();
        assert_eq!(
            sorted_span_meta,
            sorted_expected
        );
    }

    #[test]
    fn test_get_tags_parameterized() {
        let json = read_json_file("api_gateway_rest_event_parameterized.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        let tags = event.get_tags();
        let sorted_tags_array = tags
            .iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();

        let expected = HashMap::from([
            (
                "http.url".to_string(),
                "x02yirxc7a.execute-api.sa-east-1.amazonaws.com".to_string(),
            ),
            (
                "http_url_details.path".to_string(),
                "/httpapi/get".to_string(),
            ),
            ("http.method".to_string(), "GET".to_string()),
            ("http.route".to_string(), "GET /httpapi/get".to_string()),
            ("http.user_agent".to_string(), "curl/7.64.1".to_string()),
            ("http.referer".to_string(), "".to_string()),
        ]);
        let expected_sorted_array = expected
            .iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect::<Vec<String>>()
            .sort();

        assert_eq!(sorted_tags_array, expected_sorted_array);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:apigateway:us-east-1::/restapis/id/stages/$default"
        );
    }
}
