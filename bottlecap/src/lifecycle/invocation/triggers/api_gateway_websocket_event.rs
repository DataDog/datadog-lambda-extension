use crate::lifecycle::invocation::triggers::{lowercase_key, Trigger};
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
    pub stage: String,
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
        todo!()
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

    fn resolve_service_name(
        &self,
        service_mapping: &HashMap<String, String>,
        fallback: &str,
    ) -> String {
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
