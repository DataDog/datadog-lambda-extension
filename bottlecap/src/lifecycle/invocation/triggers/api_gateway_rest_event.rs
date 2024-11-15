use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{
        get_aws_partition_by_region, lowercase_key, ServiceNameResolver, Trigger,
        FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
    },
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
    fn enrich_span(&self, span: &mut Span, service_mapping: &HashMap<String, String>) {
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

        let service_name =
            self.resolve_service_name(service_mapping, &self.request_context.domain_name);

        span.name = "aws.apigateway".to_string();
        span.service = service_name;
        span.resource.clone_from(&resource);
        span.r#type = "http".to_string();
        span.start = start_time;
        span.meta.extend(HashMap::from([
            ("endpoint".to_string(), self.request_context.path.clone()),
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
            (
                "http.route".to_string(),
                self.request_context.resource_path.clone(),
            ),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        let mut tags = HashMap::from([
            (
                "http.url".to_string(),
                format!(
                    "https://{domain_name}{path}",
                    domain_name = self.request_context.domain_name,
                    path = self.request_context.path
                ),
            ),
            (
                "http.url_details.path".to_string(),
                self.request_context.path.clone(),
            ),
            (
                "http.method".to_string(),
                self.request_context.method.clone(),
            ),
            (
                "http.route".to_string(),
                self.request_context.resource_path.clone(),
            ),
            (
                "http.user_agent".to_string(),
                self.request_context.identity.user_agent.to_string(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "api-gateway".to_string(),
            ),
        ]);

        if let Some(referer) = self.headers.get("referer") {
            tags.insert("http.referer".to_string(), referer.to_string());
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

    fn get_carrier(&self) -> HashMap<String, String> {
        self.headers.clone()
    }
}

impl ServiceNameResolver for APIGatewayRestEvent {
    fn get_specific_identifier(&self) -> String {
        self.request_context.api_id.clone()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_api_gateway"
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
                time_epoch: 1_583_349_317_135,
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
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping);
        assert_eq!(span.name, "aws.apigateway");
        assert_eq!(span.service, "id.execute-api.us-east-1.amazonaws.com");
        assert_eq!(span.resource, "GET /path");
        assert_eq!(span.r#type, "http");

        assert_eq!(
            span.meta,
            HashMap::from([
                ("endpoint".to_string(), "/my/path".to_string()),
                (
                    "http.url".to_string(),
                    "https://id.execute-api.us-east-1.amazonaws.com/my/path".to_string()
                ),
                ("http.method".to_string(), "GET".to_string()),
                ("http.protocol".to_string(), "HTTP/1.1".to_string()),
                ("http.source_ip".to_string(), "IP".to_string()),
                ("http.user_agent".to_string(), "user-agent".to_string()),
                ("http.route".to_string(), "/path".to_string()),
                ("operation_name".to_string(), "aws.apigateway".to_string()),
                ("request_id".to_string(), "id=".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        let tags = event.get_tags();

        let expected = HashMap::from([
            (
                "http.url".to_string(),
                "https://id.execute-api.us-east-1.amazonaws.com/my/path".to_string(),
            ),
            ("http.url_details.path".to_string(), "/my/path".to_string()),
            ("http.method".to_string(), "GET".to_string()),
            ("http.route".to_string(), "/path".to_string()),
            ("http.user_agent".to_string(), "user-agent".to_string()),
            (
                "function_trigger.event_source".to_string(),
                "api-gateway".to_string(),
            ),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_enrich_parameterized_span() {
        let json = read_json_file("api_gateway_rest_event_parameterized.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping);
        assert_eq!(span.name, "aws.apigateway");
        assert_eq!(
            span.service,
            "mcwkra0ya4.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /user/{id}");
        assert_eq!(span.r#type, "http");
        let expected = HashMap::from([
            ("endpoint".to_string(), "/dev/user/42".to_string()),
            (
                "http.url".to_string(),
                "https://mcwkra0ya4.execute-api.sa-east-1.amazonaws.com/dev/user/42".to_string(),
            ),
            ("http.method".to_string(), "GET".to_string()),
            ("http.protocol".to_string(), "HTTP/1.1".to_string()),
            ("http.source_ip".to_string(), "76.115.124.192".to_string()),
            ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
            ("http.route".to_string(), "/user/{id}".to_string()),
            ("operation_name".to_string(), "aws.apigateway".to_string()),
            (
                "request_id".to_string(),
                "e16399f7-e984-463a-9931-745ba021a27f".to_string(),
            ),
        ]);
        assert_eq!(span.meta, expected);
    }

    #[test]
    fn test_get_tags_parameterized() {
        let json = read_json_file("api_gateway_rest_event_parameterized.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        let tags = event.get_tags();

        assert_eq!(
            tags,
            HashMap::from([
                (
                    "http.url".to_string(),
                    "https://mcwkra0ya4.execute-api.sa-east-1.amazonaws.com/dev/user/42"
                        .to_string(),
                ),
                (
                    "http.url_details.path".to_string(),
                    "/dev/user/42".to_string(),
                ),
                ("http.method".to_string(), "GET".to_string()),
                ("http.route".to_string(), "/user/{id}".to_string()),
                ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
                (
                    "function_trigger.event_source".to_string(),
                    "api-gateway".to_string()
                ),
            ])
        );
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

    #[test]
    fn test_resolve_service_name() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");

        // Priority is given to the specific key
        let specific_service_mapping = HashMap::from([
            ("id".to_string(), "specific-service".to_string()),
            (
                "lambda_api_gateway".to_string(),
                "generic-service".to_string(),
            ),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.request_context.domain_name
            ),
            "specific-service"
        );

        let generic_service_mapping = HashMap::from([(
            "lambda_api_gateway".to_string(),
            "generic-service".to_string(),
        )]);
        assert_eq!(
            event
                .resolve_service_name(&generic_service_mapping, &event.request_context.domain_name),
            "generic-service"
        );
    }
}
