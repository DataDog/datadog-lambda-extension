use super::ServiceNameResolver;
use crate::lifecycle::invocation::triggers::{Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG};
use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ALBEvent {
    request_context: RequestContext,
    http_method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    multi_value_headers: HashMap<String, Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct RequestContext {
    elb: Elb,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Elb {
    #[serde(rename = "targetGroupArn")]
    target_group_arn: String,
}

impl Trigger for ALBEvent {
    fn new(payload: Value) -> Option<Self> {
        match serde_json::from_value(payload) {
            Ok(event) => Some(event),
            Err(e) => {
                debug!("Failed to deserialize ApplicationLoadBalancer event: {}", e);
                None
            }
        }
    }

    fn is_match(payload: &Value) -> bool {
        let target_group_arn = payload
            .get("requestContext")
            .and_then(|v| v.get("elb"))
            .and_then(|v| v.get("targetGroupArn"));
        target_group_arn.is_some()
    }

    fn enrich_span(&self, _span: &mut Span, _service_mapping: &HashMap<String, String>) {}

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([
            ("http.method".to_string(), self.http_method.clone()),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "application-load-balancer".to_string(),
            ),
        ])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.request_context.elb.target_group_arn.clone()
    }

    fn is_async(&self) -> bool {
        true
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        if !self.headers.is_empty() {
            return self.headers.clone();
        }

        let mut flat_map = HashMap::new();
        for (key, array) in &self.multi_value_headers {
            if let Some(first_val) = array.first() {
                flat_map.insert(key.to_lowercase(), first_val.clone());
            }
        }
        flat_map
    }
}

impl ServiceNameResolver for ALBEvent {
    fn get_specific_identifier(&self) -> String {
        self.request_context.elb.target_group_arn.clone()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_application_load_balancer"
    }
}
