use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;
use crate::config::get_aws_partition_by_region;
use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{lowercase_key, ServiceNameResolver, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG},
};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct APIGatewayHttpEvent {
    #[serde(rename = "routeKey")]
    pub route_key: String,
    #[serde(deserialize_with = "lowercase_key")]
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
    #[serde(rename = "timeEpoch")]
    pub time_epoch: i64,
    pub http: RequestContextHTTP,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct RequestContextHTTP {
    pub method: String,
    pub path: String,
    pub protocol: String,
    #[serde(rename = "sourceIp")]
    pub source_ip: String,
    #[serde(rename = "userAgent")]
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
            && domain_name.is_some_and(|d| d.as_str().map_or(true, |s| !s.contains("lambda-url")))
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span, service_mapping: &HashMap<String, String>) {
        debug!("Enriching an Inferred Span for an API Gateway HTTP Event");
        let resource = if self.route_key.is_empty() {
            format!(
                "{http_method} {route_key}",
                http_method = self.request_context.http.method,
                route_key = self.route_key
            )
        } else {
            self.route_key.clone()
        };

        let http_url = format!(
            "https://{domain_name}{path}",
            domain_name = self.request_context.domain_name,
            path = self.request_context.http.path
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name =
            self.resolve_service_name(service_mapping, &self.request_context.domain_name);

        span.name = "aws.httpapi".to_string();
        span.service = service_name;
        span.resource.clone_from(&resource);
        span.r#type = "http".to_string();
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
            ("operation_name".to_string(), "aws.httpapi".to_string()),
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
        event.enrich_span(&mut span, &service_mapping);
        assert_eq!(span.name, "aws.httpapi");
        assert_eq!(
            span.service,
            "x02yirxc7a.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /httpapi/get");
        assert_eq!(span.r#type, "http");
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
                ("operation_name".to_string(), "aws.httpapi".to_string()),
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
        event.enrich_span(&mut span, &service_mapping);
        assert_eq!(span.name, "aws.httpapi");
        assert_eq!(
            span.service,
            "9vj54we5ih.execute-api.sa-east-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /user/{id}");
        assert_eq!(span.r#type, "http");
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
                ("operation_name".to_string(), "aws.httpapi".to_string()),
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
    fn test_resolve_service_name() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            APIGatewayHttpEvent::new(payload).expect("Failed to deserialize APIGatewayHttpEvent");

        // Priority is given to the specific key
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
