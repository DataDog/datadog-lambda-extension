use crate::config::aws::get_aws_partition_by_region;
use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{
        FUNCTION_TRIGGER_EVENT_SOURCE_TAG, ServiceNameResolver, Trigger, body::Body, lowercase_key,
        parameterize_api_resource,
    },
};
use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct APIGatewayHttpEvent {
    #[serde(default)]
    pub route_key: String,
    pub cookies: Option<Vec<String>>,
    #[serde(default)]
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    pub request_context: RequestContext,
    #[serde(default)]
    pub path_parameters: HashMap<String, String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub query_string_parameters: HashMap<String, String>,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub api_id: String,
    #[serde(default)]
    pub domain_name: String,
    #[serde(default)]
    pub time_epoch: i64,
    pub http: RequestContextHTTP,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContextHTTP {
    pub method: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub source_ip: String,
    #[serde(default)]
    pub user_agent: String,
}

impl Trigger for APIGatewayHttpEvent {
    fn new(payload: Value) -> Option<Self> {
        serde_json::from_value(payload).ok()?
    }

    fn is_match(payload: &Value) -> bool {
        let version = payload.get("version");
        let domain_name: Option<&Value> = payload
            .get("requestContext")
            .and_then(|d| d.get("domainName"));

        version.is_some_and(|v| v == "2.0")
            && payload.get("rawQueryString").is_some()
            && domain_name.is_some_and(|d| d.as_str().is_none_or(|s| !s.contains("lambda-url")))
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(
        &self,
        span: &mut Span,
        service_mapping: &HashMap<String, String>,
        aws_service_representation_enabled: bool,
    ) {
        debug!("Enriching an Inferred Span for an API Gateway HTTP Event");
        let resource = format!(
            "{http_method} {parameterized_route}",
            http_method = self.request_context.http.method,
            parameterized_route = parameterize_api_resource(self.request_context.http.path.clone())
        );
        let http_url = format!(
            "https://{domain_name}{path}",
            domain_name = self.request_context.domain_name,
            path = self.request_context.http.path
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name = self.resolve_service_name(
            service_mapping,
            &self.request_context.domain_name,
            &self.request_context.domain_name,
            aws_service_representation_enabled,
        );

        span.name = "aws.httpapi".to_string();
        span.service = service_name;
        span.resource.clone_from(&resource);
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend(HashMap::from([
            (
                "endpoint".to_string(),
                self.request_context.http.path.clone(),
            ),
            ("http.url".to_string(), http_url),
            (
                "http.method".to_string(),
                self.request_context.http.method.clone(),
            ),
            (
                "http.protocol".to_string(),
                self.request_context.http.protocol.clone(),
            ),
            (
                "http.source_ip".to_string(),
                self.request_context.http.source_ip.clone(),
            ),
            (
                "http.user_agent".to_string(),
                self.request_context.http.user_agent.clone(),
            ),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        let mut tags = HashMap::from([
            (
                "http.url".to_string(),
                format!(
                    "https://{domain_name}{path}",
                    domain_name = self.request_context.domain_name.clone(),
                    path = self.request_context.http.path.clone()
                ),
            ),
            // path and URL are full
            // /users/12345/profile
            (
                "http.url_details.path".to_string(),
                self.request_context.http.path.clone(),
            ),
            (
                "http.method".to_string(),
                self.request_context.http.method.clone(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "api-gateway".to_string(),
            ),
        ]);
        // route is parameterized
        // /users/{id}/profile
        if !self.route_key.is_empty() {
            tags.insert(
                "http.route".to_string(),
                self.route_key
                    .clone()
                    .split_whitespace()
                    .last()
                    .unwrap_or(&self.route_key.clone())
                    .to_string(),
            );
        }

        if let Some(referer) = self.headers.get("referer") {
            tags.insert("http.referer".to_string(), referer.clone());
        }

        if let Some(user_agent) = self.headers.get("user-agent") {
            tags.insert("http.user_agent".to_string(), user_agent.clone());
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
            "arn:{partition}:apigateway:{region}::/apis/{api_id}",
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

impl ServiceNameResolver for APIGatewayHttpEvent {
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
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = APIGatewayHttpEvent::new(payload)
            .expect("Failed to deserialize into APIGatewayHttpEvent");

        let expected = APIGatewayHttpEvent {
            route_key: "GET /httpapi/get".to_string(),
            cookies: None,
            headers: HashMap::from([
                ("accept".to_string(), "*/*".to_string()),
                ("content-length".to_string(), "0".to_string()),
                (
                    "host".to_string(),
                    "x02yirxc7a.execute-api.sa-east-1.amazonaws.com".to_string(),
                ),
                ("user-agent".to_string(), "curl/7.64.1".to_string()),
                (
                    "x-amzn-trace-id".to_string(),
                    "Root=1-613a52fb-4c43cfc95e0241c1471bfa05".to_string(),
                ),
                ("x-forwarded-for".to_string(), "38.122.226.210".to_string()),
                ("x-forwarded-port".to_string(), "443".to_string()),
                ("x-forwarded-proto".to_string(), "https".to_string()),
                ("x-datadog-trace-id".to_string(), "12345".to_string()),
                ("x-datadog-parent-id".to_string(), "67890".to_string()),
                ("x-datadog-sampling-priority".to_string(), "2".to_string()),
            ]),
            request_context: RequestContext {
                stage: "$default".to_string(),
                request_id: "FaHnXjKCGjQEJ7A=".to_string(),
                api_id: "x02yirxc7a".to_string(),
                domain_name: "x02yirxc7a.execute-api.sa-east-1.amazonaws.com".to_string(),
                time_epoch: 1_631_212_283_738,
                http: RequestContextHTTP {
                    method: "GET".to_string(),
                    path: "/httpapi/get".to_string(),
                    protocol: "HTTP/1.1".to_string(),
                    source_ip: "38.122.226.210".to_string(),
                    user_agent: "curl/7.64.1".to_string(),
                },
            },
            path_parameters: HashMap::default(),
            query_string_parameters: HashMap::default(),
            body: Body::default(),
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize APIGatewayHttpEvent");

        assert!(APIGatewayHttpEvent::is_match(&payload));
    }

    #[test]

    fn test_is_not_match() {
        let json = read_json_file("api_gateway_proxy_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize APIGatewayHttpEvent");
        assert!(!APIGatewayHttpEvent::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");
        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);
        assert_eq!(span.name, "aws.httpapi");
        assert_eq!(
            span.service,
            "x02yirxc7a.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /httpapi/get");
        assert_eq!(span.r#type, "web");
        assert_eq!(
            span.meta,
            HashMap::from([
                ("endpoint".to_string(), "/httpapi/get".to_string()),
                (
                    "http.url".to_string(),
                    "https://x02yirxc7a.execute-api.sa-east-1.amazonaws.com/httpapi/get"
                        .to_string()
                ),
                ("http.method".to_string(), "GET".to_string()),
                ("http.protocol".to_string(), "HTTP/1.1".to_string()),
                ("http.source_ip".to_string(), "38.122.226.210".to_string()),
                ("http.user_agent".to_string(), "curl/7.64.1".to_string()),
                ("request_id".to_string(), "FaHnXjKCGjQEJ7A=".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");
        let tags = event.get_tags();
        let expected = HashMap::from([
            (
                "http.url".to_string(),
                "https://x02yirxc7a.execute-api.sa-east-1.amazonaws.com/httpapi/get".to_string(),
            ),
            (
                "http.url_details.path".to_string(),
                "/httpapi/get".to_string(),
            ),
            ("http.method".to_string(), "GET".to_string()),
            ("http.route".to_string(), "/httpapi/get".to_string()),
            ("http.user_agent".to_string(), "curl/7.64.1".to_string()),
            (
                "function_trigger.event_source".to_string(),
                "api-gateway".to_string(),
            ),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]

    fn test_enrich_span_parameterized() {
        let json = read_json_file("api_gateway_http_event_parameterized.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");
        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);
        assert_eq!(span.name, "aws.httpapi");
        assert_eq!(
            span.service,
            "9vj54we5ih.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /user/{user_id}");
        assert_eq!(span.r#type, "web");
        assert_eq!(
            span.meta,
            HashMap::from([
                ("endpoint".to_string(), "/user/42".to_string()),
                (
                    "http.url".to_string(),
                    "https://9vj54we5ih.execute-api.sa-east-1.amazonaws.com/user/42".to_string()
                ),
                ("http.method".to_string(), "GET".to_string()),
                ("http.protocol".to_string(), "HTTP/1.1".to_string()),
                ("http.source_ip".to_string(), "76.115.124.192".to_string()),
                ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
                ("request_id".to_string(), "Ur2JtjEfGjQEPOg=".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags_parameterized() {
        let json = read_json_file("api_gateway_http_event_parameterized.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");
        let tags = event.get_tags();

        let expected = HashMap::from([
            (
                "http.url".to_string(),
                "https://9vj54we5ih.execute-api.sa-east-1.amazonaws.com/user/42".to_string(),
            ),
            ("http.url_details.path".to_string(), "/user/42".to_string()),
            ("http.method".to_string(), "GET".to_string()),
            ("http.route".to_string(), "/user/{id}".to_string()),
            ("http.user_agent".to_string(), "curl/8.1.2".to_string()),
            (
                "function_trigger.event_source".to_string(),
                "api-gateway".to_string(),
            ),
        ]);
        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");
        assert_eq!(
            event.get_arn("sa-east-1"),
            "arn:aws:apigateway:sa-east-1::/restapis/x02yirxc7a/stages/$default"
        );
    }

    #[test]
    fn test_get_dd_resource_key() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");
        assert_eq!(
            event.get_dd_resource_key("sa-east-1"),
            Some("arn:aws:apigateway:sa-east-1::/apis/x02yirxc7a".to_string())
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_enabled() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");

        // Test 1: Specific mapping takes priority
        let specific_service_mapping = HashMap::from([
            ("x02yirxc7a".to_string(), "specific-service".to_string()),
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
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");

        // Test 1: With specific mapping - still respects mapping
        let specific_service_mapping = HashMap::from([
            ("x02yirxc7a".to_string(), "specific-service".to_string()),
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
