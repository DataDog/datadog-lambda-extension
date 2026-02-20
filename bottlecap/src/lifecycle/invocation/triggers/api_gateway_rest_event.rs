use crate::config::aws::get_aws_partition_by_region;
use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{
        FUNCTION_TRIGGER_EVENT_SOURCE_TAG, ServiceNameResolver, Trigger, body::Body, lowercase_key,
        parameterize_api_resource, serde_utils::nullable_map,
    },
};
use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct APIGatewayRestEvent {
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    #[serde(deserialize_with = "lowercase_key")]
    pub multi_value_headers: HashMap<String, Vec<String>>,
    #[serde(default)]
    #[serde(deserialize_with = "nullable_map")]
    #[serde(rename = "multiValueQueryStringParameters")]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub query_parameters: HashMap<String, Vec<String>>,
    #[serde(default)]
    #[serde(deserialize_with = "nullable_map")]
    pub path_parameters: HashMap<String, String>,
    pub request_context: RequestContext,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub stage: String,
    pub request_id: String,
    pub api_id: String,
    pub domain_name: String,
    #[serde(rename = "requestTimeEpoch")]
    pub time_epoch: i64,
    #[serde(rename = "httpMethod")]
    pub method: String,
    pub resource_path: String,
    pub path: String,
    pub protocol: String,
    pub identity: Identity,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub source_ip: String,
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
    fn enrich_span(
        &self,
        span: &mut Span,
        service_mapping: &HashMap<String, String>,
        aws_service_representation_enabled: bool,
    ) {
        debug!("Enriching an Inferred Span for an API Gateway REST Event");
        let resource = format!(
            "{http_method} {path}",
            http_method = self.request_context.method,
            path = parameterize_api_resource(self.request_context.path.clone())
        );
        let http_url = format!(
            "https://{domain_name}{path}",
            domain_name = self.request_context.domain_name,
            path = self.request_context.path
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name = self.resolve_service_name(
            service_mapping,
            &self.request_context.domain_name,
            &self.request_context.domain_name,
            aws_service_representation_enabled,
        );

        span.name = "aws.apigateway".to_string();
        span.service = service_name;
        span.resource = resource;
        span.r#type = "web".to_string();
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
                self.request_context.identity.user_agent.clone(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "api-gateway".to_string(),
            ),
        ]);

        if let Some(referer) = self.headers.get("referer") {
            tags.insert("http.referer".to_string(), referer.clone());
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

    fn get_dd_resource_key(&self, region: &str) -> Option<String> {
        if self.request_context.api_id.is_empty() {
            return None;
        }

        let partition = get_aws_partition_by_region(region);
        Some(format!(
            "arn:{partition}:apigateway:{region}::/restapis/{api_id}",
            partition = partition,
            region = region,
            api_id = self.request_context.api_id
        ))
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
    fn test_parameterize_resource() {
        // Test case with numeric IDs
        assert_eq!(
            parameterize_api_resource("/users/12345/friends/67890".to_string()),
            "/users/{user_id}/friends/{friend_id}"
        );

        assert_eq!(
            parameterize_api_resource("/dev/proxy_route/users/12345/friends/67890".to_string()),
            "/dev/proxy_route/users/{user_id}/friends/{friend_id}"
        );

        // Test case with already parameterized path
        assert_eq!(
            parameterize_api_resource("/users/{user_id}/profile".to_string()),
            "/users/{user_id}/profile"
        );

        // Test case with mixed segments
        assert_eq!(
            parameterize_api_resource("/api/v1/users/12345/settings".to_string()),
            "/api/v1/users/{user_id}/settings"
        );

        // Test case with UUIDs
        assert_eq!(
            parameterize_api_resource(
                "/orders/123e4567-e89b-12d3-a456-426614174000/items".to_string()
            ),
            "/orders/{order_id}/items"
        );
    }

    #[test]
    fn test_new() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = APIGatewayRestEvent::new(payload)
            .expect("Failed to deserialize into APIGatewayRestEvent");

        let expected = APIGatewayRestEvent {
            headers: HashMap::from([
                ("header1".to_string(), "value1".to_string()),
                ("header2".to_string(), "value2".to_string()),
            ]),
            multi_value_headers: HashMap::from([
                ("header1".to_string(), vec!["value1".to_string()]),
                (
                    "header2".to_string(),
                    vec!["value1".to_string(), "value2".to_string()],
                ),
            ]),
            query_parameters: HashMap::from([
                (
                    "parameter1".to_string(),
                    vec!["value1".to_string(), "value2".to_string()],
                ),
                ("parameter2".to_string(), vec!["value".to_string()]),
            ]),
            path_parameters: HashMap::default(),
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
            body: Body {
                body: Some("Hello from Lambda!".to_string()),
                is_base64_encoded: false,
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
        event.enrich_span(&mut span, &service_mapping, true);
        assert_eq!(span.name, "aws.apigateway");
        assert_eq!(span.service, "id.execute-api.us-east-1.amazonaws.com");
        assert_eq!(span.resource, "GET /my/path");
        assert_eq!(span.r#type, "web");

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
        event.enrich_span(&mut span, &service_mapping, true);
        assert_eq!(span.name, "aws.apigateway");
        assert_eq!(
            span.service,
            "mcwkra0ya4.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /dev/user/{user_id}/id/{id}");
        assert_eq!(span.r#type, "web");
        let expected = HashMap::from([
            ("endpoint".to_string(), "/dev/user/42/id/50".to_string()),
            (
                "http.url".to_string(),
                "https://mcwkra0ya4.execute-api.sa-east-1.amazonaws.com/dev/user/42/id/50"
                    .to_string(),
            ),
            ("http.method".to_string(), "GET".to_string()),
            ("http.protocol".to_string(), "HTTP/1.1".to_string()),
            ("http.source_ip".to_string(), "76.115.124.192".to_string()),
            ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
            ("http.route".to_string(), "/user/{id}".to_string()),
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
                    "https://mcwkra0ya4.execute-api.sa-east-1.amazonaws.com/dev/user/42/id/50"
                        .to_string(),
                ),
                (
                    "http.url_details.path".to_string(),
                    "/dev/user/42/id/50".to_string(),
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
    fn test_get_dd_resource_key() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");
        assert_eq!(
            event.get_dd_resource_key("us-east-1"),
            Some("arn:aws:apigateway:us-east-1::/restapis/id".to_string())
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_enabled() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");

        // Test 1: Specific mapping takes priority
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
                &event.request_context.domain_name,
                &event.request_context.domain_name,
                true // aws_service_representation_enabled
            ),
            "specific-service"
        );

        // Test 2: Generic mapping is used when specific not found
        let generic_service_mapping = HashMap::from([(
            "lambda_api_gateway".to_string(),
            "generic-service".to_string(),
        )]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.request_context.domain_name,
                &event.request_context.domain_name,
                true // aws_service_representation_enabled
            ),
            "generic-service"
        );

        // Test 3: When no mapping exists, uses instance name (domain_name)
        let empty_mapping = HashMap::new();
        assert_eq!(
            event.resolve_service_name(
                &empty_mapping,
                &event.request_context.domain_name,
                &event.request_context.domain_name,
                true // aws_service_representation_enabled
            ),
            event.request_context.domain_name // instance name
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_disabled() {
        let json = read_json_file("api_gateway_rest_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayRestEvent::new(payload).expect("Failed to deserialize APIGatewayRestEvent");

        // Test 1: With specific mapping - still respects mapping
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
                &event.request_context.domain_name,
                &event.request_context.domain_name,
                false // aws_service_representation_enabled = false
            ),
            "specific-service"
        );

        // Test 2: With generic mapping - still respects mapping
        let generic_service_mapping = HashMap::from([(
            "lambda_api_gateway".to_string(),
            "generic-service".to_string(),
        )]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.request_context.domain_name,
                &event.request_context.domain_name,
                false // aws_service_representation_enabled = false
            ),
            "generic-service"
        );

        // Test 3: When no mapping exists, uses fallback value (domain_name)
        let empty_mapping = HashMap::new();
        assert_eq!(
            event.resolve_service_name(
                &empty_mapping,
                &event.request_context.domain_name,
                &event.request_context.domain_name,
                false // aws_service_representation_enabled = false
            ),
            event.request_context.domain_name // fallback value
        );
    }
}
