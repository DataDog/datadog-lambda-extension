use super::ServiceNameResolver;
use crate::lifecycle::invocation::triggers::{FUNCTION_TRIGGER_EVENT_SOURCE_TAG, Trigger};
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

    fn enrich_span(
        &self,
        _span: &mut Span,
        _service_mapping: &HashMap<String, String>,
        _aws_service_representation_enabled: bool,
    ) {
    }

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
        // arn is of format: arn:aws:elasticloadbalancing:<region>:<account-id>:targetgroup/<alb-name>/<some-uid>
        let arn = &self.request_context.elb.target_group_arn;

        arn.split('/').nth(1).unwrap_or_default().to_string()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_application_load_balancer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::api_gateway_http_event::APIGatewayHttpEvent;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("application_load_balancer.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = ALBEvent::new(payload).expect("Failed to deserialize into ALBEvent");

        let expected = ALBEvent {
            request_context: RequestContext {
                elb: Elb {
                    target_group_arn: "arn:aws:elasticloadbalancing:us-east-1:1234567890:targetgroup/nhulston-alb-test/dcabb42f66a496e0".to_string(),
                }
            },
            http_method: "GET".to_string(),
            headers: HashMap::from([
                ("accept".to_string(), "*/*".to_string()),
                ("accept-encoding".to_string(), "gzip, deflate".to_string()),
                ("accept-language".to_string(), "*".to_string()),
                ("connection".to_string(), "keep-alive".to_string()),
                ("host".to_string(), "nhulston-test-0987654321.us-east-1.elb.amazonaws.com".to_string()),
                ("sec-fetch-mode".to_string(), "cors".to_string()),
                ("traceparent".to_string(), "00-68126c4300000000125a7f065cf9a530-1c6dcc8ab8a6e99d-01".to_string()),
                ("tracestate".to_string(), "dd=t.dm:-0;t.tid:68126c4300000000;s:1;p:1c6dcc8ab8a6e99d".to_string()),
                ("user-agent".to_string(), "node".to_string()),
                ("x-amzn-trace-id".to_string(), "Root=1-68126c45-01b175997ab51c4c47a2d643".to_string()),
                ("x-datadog-parent-id".to_string(), "1234567890".to_string()),
                ("x-datadog-sampling-priority".to_string(), "1".to_string()),
                ("x-datadog-tags".to_string(), "_dd.p.tid=68126c4300000000,_dd.p.dm=-0".to_string()),
                ("x-datadog-trace-id".to_string(), "0987654321".to_string()),
                ("x-forwarded-for".to_string(), "18.204.55.6".to_string()),
                ("x-forwarded-port".to_string(), "80".to_string()),
                ("x-forwarded-proto".to_string(), "http".to_string()),
            ]),
            multi_value_headers: HashMap::default(),
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_new_multivalue_headers() {
        let json = read_json_file("application_load_balancer_multivalue_headers.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = ALBEvent::new(payload).expect("Failed to deserialize into ALBEvent");

        assert_eq!(
            result.multi_value_headers.get("accept"),
            Some(&vec!["*/*".to_string()])
        );
        assert_eq!(
            result.multi_value_headers.get("x-datadog-trace-id"),
            Some(&vec!["0987654321".to_string()])
        );
        assert_eq!(
            result.multi_value_headers.get("x-datadog-parent-id"),
            Some(&vec!["1234567890".to_string()])
        );
        assert_eq!(
            result
                .multi_value_headers
                .get("x-datadog-sampling-priority"),
            Some(&vec!["1".to_string()])
        );
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("application_load_balancer.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize ALBEvent");

        assert!(ALBEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("api_gateway_proxy_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize APIGatewayHttpEvent");
        assert!(!APIGatewayHttpEvent::is_match(&payload));
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("application_load_balancer.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = ALBEvent::new(payload).expect("Failed to deserialize ALBEvent");
        let tags = event.get_tags();
        let expected = HashMap::from([
            ("http.method".to_string(), "GET".to_string()),
            (
                "function_trigger.event_source".to_string(),
                "application-load-balancer".to_string(),
            ),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("application_load_balancer.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = ALBEvent::new(payload).expect("Failed to deserialize ALBEvent");
        assert_eq!(
            event.get_arn("sa-east-1"),
            "arn:aws:elasticloadbalancing:us-east-1:1234567890:targetgroup/nhulston-alb-test/dcabb42f66a496e0"
        );
    }

    #[test]
    fn test_resolve_service_name() {
        let json = read_json_file("application_load_balancer.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = ALBEvent::new(payload).expect("Failed to deserialize ALBEvent");

        // Priority is given to the specific key
        let specific_service_mapping = HashMap::from([
            (
                "nhulston-alb-test".to_string(),
                "specific-service".to_string(),
            ),
            (
                "lambda_application_load_balancer".to_string(),
                "generic-service".to_string(),
            ),
        ]);

        assert_eq!(
            event.resolve_service_name(
                &specific_service_mapping,
                &event.request_context.elb.target_group_arn,
                "lambda_application_load_balancer",
                true
            ),
            "specific-service"
        );

        let generic_service_mapping = HashMap::from([(
            "lambda_application_load_balancer".to_string(),
            "generic-service".to_string(),
        )]);
        assert_eq!(
            event.resolve_service_name(
                &generic_service_mapping,
                &event.request_context.elb.target_group_arn,
                "lambda_application_load_balancer",
                true
            ),
            "generic-service"
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("application_load_balancer.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = ALBEvent::new(payload).expect("Failed to deserialize ALBEvent");
        let carrier = event.get_carrier();

        assert_eq!(
            carrier.get("x-datadog-trace-id"),
            Some(&"0987654321".to_string())
        );
        assert_eq!(
            carrier.get("x-datadog-parent-id"),
            Some(&"1234567890".to_string())
        );
        assert_eq!(
            carrier.get("x-datadog-sampling-priority"),
            Some(&"1".to_string())
        );
    }

    #[test]
    fn test_get_carrier_multivalue_headers() {
        let json = read_json_file("application_load_balancer_multivalue_headers.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = ALBEvent::new(payload).expect("Failed to deserialize ALBEvent");
        let carrier = event.get_carrier();

        assert_eq!(
            carrier.get("x-datadog-trace-id"),
            Some(&"0987654321".to_string())
        );
        assert_eq!(
            carrier.get("x-datadog-parent-id"),
            Some(&"1234567890".to_string())
        );
        assert_eq!(
            carrier.get("x-datadog-sampling-priority"),
            Some(&"1".to_string())
        );
    }
}
