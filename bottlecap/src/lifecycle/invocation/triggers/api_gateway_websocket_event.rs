use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{lowercase_key, Trigger},
};
use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use super::ServiceNameResolver;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct APIGatewayWebSocketEvent {
    #[serde(deserialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    #[serde(rename = "requestContext")]
    pub request_context: RequestContext,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct RequestContext {
    #[serde(rename = "routeKey")]
    pub route_key: String,
    #[serde(rename = "domainName")]
    pub domain_name: String,
    #[serde(rename = "requestTimeEpoch")]
    pub time_epoch: i64,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "apiId")]
    pub api_id: String,
    pub stage: String,
    #[serde(rename = "connectionId")]
    pub connection_id: String,
    #[serde(rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "messageDirection")]
    pub message_direction: String,
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
    fn enrich_span(&self, span: &mut Span, service_mapping: &HashMap<String, String>) {
        debug!("Enriching an Inferred Span for an API Gateway WebSocket Event");
        let resource = &self.request_context.route_key;
        let http_url = format!(
            "{domain_name}{route_key}",
            domain_name = self.request_context.domain_name,
            route_key = self.request_context.route_key
        );
        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;

        let service_name =
            self.resolve_service_name(service_mapping, &self.request_context.domain_name);

        span.name = "aws.apigateway".to_string();
        span.service = service_name;
        span.resource.clone_from(&resource);
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
            ("operation_name".to_string(), "aws.apigateway".to_string()),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
            ("apiid".to_string(), self.request_context.api_id.clone()),
            ("apiname".to_string(), self.request_context.api_id.clone()),
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
        todo!()
    }

    fn get_arn(&self, region: &str) -> String {
        todo!()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        todo!()
    }

    fn is_async(&self) -> bool {
        todo!()
    }
}

impl ServiceNameResolver for APIGatewayWebSocketEvent {
    fn get_specific_identifier(&self) -> String {
        todo!()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_api_gateway"
    }
}
