use crate::{
    config::aws::get_aws_partition_by_region,
    lifecycle::invocation::{
        processor::MS_TO_NS,
        triggers::{FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger, body::Body, lowercase_key},
    },
};
use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use super::ServiceNameResolver;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct APIGatewayWebSocketEvent {
    pub path: Option<String>,
    #[serde(deserialize_with = "lowercase_key", default)]
    pub headers: HashMap<String, String>,
    #[serde(deserialize_with = "lowercase_key", default)]
    pub multi_value_headers: HashMap<String, Vec<String>>,
    #[serde(default)]
    #[serde(rename = "multiValueQueryStringParameters")]
    pub query_parameters: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub path_parameters: HashMap<String, String>,
    pub request_context: RequestContext,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub route_key: String,
    pub domain_name: String,
    #[serde(rename = "requestTimeEpoch")]
    pub time_epoch: i64,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_method: Option<String>,
    pub api_id: String,
    pub stage: String,
    pub connection_id: String,
    pub event_type: String,
    pub message_direction: String,
    pub identity: Identity,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub source_ip: Option<String>,
}

impl Trigger for APIGatewayWebSocketEvent {
    fn new(payload: Value) -> Option<Self> {
        match serde_json::from_value(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize APIGatewayWebSocketEvent: {}", e);
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        let message_direction = payload
            .get("requestContext")
            .and_then(|v| v.get("messageDirection"));
        message_direction.is_some()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(
        &self,
        span: &mut Span,
        service_mapping: &HashMap<String, String>,
        aws_service_representation_enabled: bool,
    ) {
        debug!("Enriching an Inferred Span for an API Gateway WebSocket Event");
        let resource = &self.request_context.route_key;
        let http_url = format!(
            "https://{domain_name}{route_key}",
            domain_name = self.request_context.domain_name,
            route_key = self.request_context.route_key
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name = self.resolve_service_name(
            service_mapping,
            &self.request_context.domain_name,
            "api_gateway_websocket",
            aws_service_representation_enabled,
        );

        span.name = "aws.apigateway".to_string();
        span.service = service_name;
        span.resource.clone_from(resource);
        span.r#type = "web".to_string();
        span.start = start_time;
        span.meta.extend(HashMap::from([
            (
                "endpoint".to_string(),
                self.request_context.route_key.clone(),
            ),
            (
                "resource_names".to_string(),
                self.request_context.route_key.clone(),
            ),
            ("http.url".to_string(), http_url),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
            ("apiid".to_string(), self.request_context.api_id.clone()),
            ("stage".to_string(), self.request_context.stage.clone()),
            (
                "connection_id".to_string(),
                self.request_context.connection_id.clone(),
            ),
            (
                "event_type".to_string(),
                self.request_context.event_type.clone(),
            ),
            (
                "message_direction".to_string(),
                self.request_context.message_direction.clone(),
            ),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        let mut tags = HashMap::from([
            (
                "http.url".to_string(),
                format!(
                    "https://{domain_name}{route_key}",
                    domain_name = self.request_context.domain_name,
                    route_key = self.request_context.route_key
                ),
            ),
            (
                "http.url_details.path".to_string(),
                self.request_context.route_key.clone(),
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
            "arn:{partition}:apigateway:{region}::/apis/{api_id}/stages/{stage}",
            partition = partition,
            region = region,
            api_id = self.request_context.api_id,
            stage = self.request_context.stage
        )
    }

    fn is_async(&self) -> bool {
        // WebSocket events are always async
        true
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        self.headers.clone()
    }
}

impl ServiceNameResolver for APIGatewayWebSocketEvent {
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
    fn test_new_connect_event() {
        let json = read_json_file("api_gateway_websocket_connect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize into APIGatewayWebSocketEvent");

        let expected = APIGatewayWebSocketEvent {
            path: None,
            headers: HashMap::new(),
            multi_value_headers: HashMap::new(),
            query_parameters: HashMap::new(),
            path_parameters: HashMap::new(),
            request_context: RequestContext {
                route_key: "hello".to_string(),
                domain_name: "85fj5nw29d.execute-api.eu-west-1.amazonaws.com".to_string(),
                time_epoch: 1_666_633_666_203,
                request_id: "ahVmYGOMmjQFhyg=".to_string(),
                http_method: None,
                api_id: "85fj5nw29d".to_string(),
                stage: "dev".to_string(),
                connection_id: "ahVWscZqmjQCI1w=".to_string(),
                event_type: "MESSAGE".to_string(),
                message_direction: "IN".to_string(),
                identity: Identity {
                    source_ip: Some("24.193.182.233".to_string()),
                },
            },
            body: Body {
                body: Some(r#"{"action": "hello", "message":"in"}"#.to_string()),
                is_base64_encoded: false,
            },
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_new_message_event() {
        let json = read_json_file("api_gateway_websocket_message_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize into APIGatewayWebSocketEvent");

        let expected = APIGatewayWebSocketEvent {
            path: None,
            headers: HashMap::new(),
            multi_value_headers: HashMap::new(),
            query_parameters: HashMap::new(),
            path_parameters: HashMap::new(),
            request_context: RequestContext {
                route_key: "hello".to_string(),
                domain_name: "85fj5nw29d.execute-api.eu-west-1.amazonaws.com".to_string(),
                time_epoch: 1_666_633_666_203,
                request_id: "ahVmYGOMmjQFhyg=".to_string(),
                http_method: None,
                api_id: "85fj5nw29d".to_string(),
                stage: "dev".to_string(),
                connection_id: "ahVWscZqmjQCI1w=".to_string(),
                event_type: "MESSAGE".to_string(),
                message_direction: "IN".to_string(),
                identity: Identity {
                    source_ip: Some("24.193.182.233".to_string()),
                },
            },
            body: Body {
                body: Some(r#"{"action": "hello", "message":"in"}"#.to_string()),
                is_base64_encoded: false,
            },
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_new_disconnect_event() {
        let json = read_json_file("api_gateway_websocket_disconnect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize into APIGatewayWebSocketEvent");

        let expected = APIGatewayWebSocketEvent {
            path: None,
            headers: HashMap::new(),
            multi_value_headers: HashMap::new(),
            query_parameters: HashMap::new(),
            path_parameters: HashMap::new(),
            request_context: RequestContext {
                route_key: "hello".to_string(),
                domain_name: "85fj5nw29d.execute-api.eu-west-1.amazonaws.com".to_string(),
                time_epoch: 1_666_633_666_203,
                request_id: "ahVmYGOMmjQFhyg=".to_string(),
                http_method: None,
                api_id: "85fj5nw29d".to_string(),
                stage: "production".to_string(),
                connection_id: "ahVWscZqmjQCI1w=".to_string(),
                event_type: "DISCONNECT".to_string(), // Note: The example payload shows MESSAGE event type, not DISCONNECT
                message_direction: "IN".to_string(),
                identity: Identity {
                    source_ip: Some("24.193.182.233".to_string()),
                },
            },
            body: Body {
                body: Some(r#"{"action": "hello", "message":"in"}"#.to_string()),
                is_base64_encoded: false,
            },
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("api_gateway_websocket_connect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        assert!(APIGatewayWebSocketEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("api_gateway_http_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        assert!(!APIGatewayWebSocketEvent::is_match(&payload));

        let json = read_json_file("api_gateway_proxy_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        assert!(!APIGatewayWebSocketEvent::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("api_gateway_websocket_connect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize APIGatewayWebSocketEvent");

        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping, true);

        assert_eq!(span.name, "aws.apigateway");
        assert_eq!(
            span.service,
            "85fj5nw29d.execute-api.eu-west-1.amazonaws.com"
        );
        assert_eq!(span.resource, "hello");
        assert_eq!(span.r#type, "web");
        assert_eq!(
            span.meta,
            HashMap::from([
                ("endpoint".to_string(), "hello".to_string()),
                ("resource_names".to_string(), "hello".to_string()),
                (
                    "http.url".to_string(),
                    "https://85fj5nw29d.execute-api.eu-west-1.amazonaws.comhello".to_string()
                ),
                ("request_id".to_string(), "ahVmYGOMmjQFhyg=".to_string()),
                ("apiid".to_string(), "85fj5nw29d".to_string()),
                ("stage".to_string(), "dev".to_string()),
                ("connection_id".to_string(), "ahVWscZqmjQCI1w=".to_string()),
                ("event_type".to_string(), "MESSAGE".to_string()),
                ("message_direction".to_string(), "IN".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("api_gateway_websocket_connect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize APIGatewayWebSocketEvent");

        let tags = event.get_tags();
        let expected = HashMap::from([
            (
                "http.url".to_string(),
                "https://85fj5nw29d.execute-api.eu-west-1.amazonaws.comhello".to_string(),
            ),
            ("http.url_details.path".to_string(), "hello".to_string()),
            (
                "function_trigger.event_source".to_string(),
                "api-gateway".to_string(),
            ),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("api_gateway_websocket_connect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize APIGatewayWebSocketEvent");

        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:apigateway:us-east-1::/apis/85fj5nw29d/stages/dev"
        );
    }

    #[test]
    fn test_resolve_service_name_with_representation_enabled() {
        let json = read_json_file("api_gateway_websocket_connect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize APIGatewayWebSocketEvent");

        // Test 1: Specific mapping takes priority
        let specific_service_mapping = HashMap::from([
            ("85fj5nw29d".to_string(), "specific-service".to_string()),
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
        let json = read_json_file("api_gateway_websocket_connect_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = APIGatewayWebSocketEvent::new(payload)
            .expect("Failed to deserialize APIGatewayWebSocketEvent");

        // Test 1: With specific mapping - still respects mapping
        let specific_service_mapping = HashMap::from([
            ("85fj5nw29d".to_string(), "specific-service".to_string()),
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
