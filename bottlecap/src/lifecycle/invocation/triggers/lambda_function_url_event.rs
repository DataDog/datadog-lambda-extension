use std::{collections::HashMap, env};

use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lifecycle::invocation::{
    processor::MS_TO_NS,
    triggers::{lowercase_key, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG},
};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LambdaFunctionUrlEvent {
    #[serde(serialize_with = "lowercase_key")]
    pub headers: HashMap<String, String>,
    #[serde(rename = "requestContext")]
    pub request_context: RequestContext,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct RequestContext {
    pub http: Http,
    #[serde(rename = "accountId")]
    pub account_id: String,
    #[serde(rename = "domainName")]
    pub domain_name: String,
    #[serde(rename = "timeEpoch")]
    pub time_epoch: i64,
    #[serde(rename = "requestId")]
    pub request_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Http {
    pub method: String,
    pub path: String,
    pub protocol: String,
    #[serde(rename = "sourceIp")]
    pub source_ip: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

impl Trigger for LambdaFunctionUrlEvent {
    fn new(payload: serde_json::Value) -> Option<Self>
    where
        Self: Sized,
    {
        serde_json::from_value(payload).ok()?
    }

    fn is_match(payload: &serde_json::Value) -> bool
    where
        Self: Sized,
    {
        payload
            .get("requestContext")
            .and_then(|rc| rc.get("domainName"))
            .and_then(Value::as_str)
            .map_or(false, |dn| dn.contains("lambda-url"))
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span) {
        let resource = format!(
            "{} {}",
            self.request_context.http.method, self.request_context.http.path
        );

        let http_url = format!(
            "https://{domain_name}{path}",
            domain_name = self.request_context.domain_name.clone(),
            path = self.request_context.http.path.clone()
        );

        let start_time = (self.request_context.time_epoch as f64 * MS_TO_NS) as i64;
        // todo: service mapping and peer service
        let service_name = self.request_context.domain_name.clone();

        span.name = String::from("aws.lambda.url");
        span.service = service_name;
        span.resource = resource;
        span.r#type = String::from("http");
        span.start = start_time;
        span.meta.extend([
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
                "http.user_agent".to_string(),
                self.request_context.http.user_agent.clone(),
            ),
            (
                "http.source_ip".to_string(),
                self.request_context.http.source_ip.clone(),
            ),
            (
                "http.protocol".to_string(),
                self.request_context.http.protocol.clone(),
            ),
            ("operation_name".to_string(), "aws.lambda.url".to_string()),
            (
                "request_id".to_string(),
                self.request_context.request_id.clone(),
            ),
        ]);
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
                "http.user_agent".to_string(),
                self.request_context.http.user_agent.clone(),
            ),
            (
                FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                "lambda-function-url".to_string(),
            ),
        ]);

        if let Some(referer) = self.headers.get("referer") {
            tags.insert("http.referer".to_string(), referer.clone());
        }

        tags
    }

    fn get_arn(&self, region: &str) -> String {
        let function_name = env::var("AWS_LAMBDA_FUNCTION_NAME").unwrap_or_default();
        format!(
            "arn:aws:lambda:{region}:{}:url:{}",
            self.request_context.account_id, function_name
        )
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        self.headers.clone()
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
        let json = read_json_file("lambda_function_url_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = LambdaFunctionUrlEvent::new(payload)
            .expect("Failed to deserialize into LambdaFunctionUrlEvent");

        let expected = LambdaFunctionUrlEvent {
            headers: HashMap::from([
                ("accept".to_string(), "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.9".to_string()),
                ("accept-language".to_string(), "en-US,en;q=0.9".to_string()),
                ("accept-encoding".to_string(), "gzip, deflate, br".to_string()),
                ("sec-fetch-mode".to_string(), "navigate".to_string()),
                ("sec-fetch-site".to_string(), "none".to_string()),
                ("sec-fetch-user".to_string(), "?1".to_string()),
                ("sec-fetch-dest".to_string(), "document".to_string()),
                ("sec-ch-ua".to_string(), "\"Google Chrome\";v=\"95\", \"Chromium\";v=\"95\", \";Not A Brand\";v=\"99\"".to_string()),
                ("sec-ch-ua-platform".to_string(), "\"macOS\"".to_string()),
                ("sec-ch-ua-mobile".to_string(), "?0".to_string()),
                ("upgrade-insecure-requests".to_string(), "1".to_string()),
                (
                    "accept-language".to_string(),
                    "en-US,en;q=0.9".to_string(),
                ),
                ("user-agent".to_string(), "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/95.0.4638.69 Safari/537.36".to_string()),
                (
                    "x-amzn-trace-id".to_string(),
                    "Root=1-61953929-1ec00c3011062a48477b169e".to_string(),
                ),
                ("x-forwarded-for".to_string(), "71.195.30.42".to_string()),
                ("x-forwarded-port".to_string(), "443".to_string()),
                ("x-forwarded-proto".to_string(), "https".to_string()),
                ("pragma".to_string(), "no-cache".to_string()),
                ("cache-control".to_string(), "no-cache".to_string()),
                ("host".to_string(), "a8hyhsshac.lambda-url.eu-south-1.amazonaws.com".to_string()),

            ]),
            request_context: RequestContext {
                request_id: String::from("ec4d58f8-2b8b-4ceb-a1d5-2be7bff58505"),
                time_epoch: 1637169449721,
                http: Http {
                    method: String::from("GET"),
                    path: String::from("/"),
                    protocol: String::from("HTTP/1.1"),
                    source_ip: String::from("71.195.30.42"),
                    user_agent: String::from("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/95.0.4638.69 Safari/537.36"),
                },
                account_id: String::from("601427279990"),
                domain_name: String::from("a8hyhsshac.lambda-url.eu-south-1.amazonaws.com"),
            },
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("lambda_function_url_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize LambdaFunctionUrlEvent");

        assert!(LambdaFunctionUrlEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("api_gateway_proxy_event.json");
        let payload =
            serde_json::from_str(&json).expect("Failed to deserialize LambdaFunctionUrlEvent");
        assert!(!LambdaFunctionUrlEvent::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("lambda_function_url_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = LambdaFunctionUrlEvent::new(payload)
            .expect("Failed to deserialize LambdaFunctionUrlEvent");
        let mut span = Span::default();
        event.enrich_span(&mut span);
        assert_eq!(span.name, "aws.lambda.url");
        assert_eq!(
            span.service,
            "a8hyhsshac.lambda-url.eu-south-1.amazonaws.com"
        );
        assert_eq!(span.resource, "GET /");
        assert_eq!(span.r#type, "http");
        assert_eq!(
            span.meta,
            HashMap::from([
                ("http.protocol".to_string(), "HTTP/1.1".to_string()),
                ("http.source_ip".to_string(), "71.195.30.42".to_string()),
                ("operation_name".to_string(), "aws.lambda.url".to_string()),
                ("request_id".to_string(), "ec4d58f8-2b8b-4ceb-a1d5-2be7bff58505".to_string()),
                ("http.url".to_string(), "https://a8hyhsshac.lambda-url.eu-south-1.amazonaws.com/".to_string()),
                ("http.method".to_string(), "GET".to_string()),
                ("http.user_agent".to_string(), "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/95.0.4638.69 Safari/537.36".to_string()),
                ("endpoint".to_string(), "/".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("lambda_function_url_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = LambdaFunctionUrlEvent::new(payload)
            .expect("Failed to deserialize LambdaFunctionUrlEvent");
        let tags = event.get_tags();
        let expected = HashMap::from([
            ("function_trigger.event_source".to_string(), "lambda-function-url".to_string()),
            ("http.method".to_string(), "GET".to_string()),
            ("http.url_details.path".to_string(), "/".to_string()),
            ("http.user_agent".to_string(), "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/95.0.4638.69 Safari/537.36".to_string()),
            ("http.url".to_string(), "https://a8hyhsshac.lambda-url.eu-south-1.amazonaws.com/".to_string()),
        ]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        env::set_var("AWS_LAMBDA_FUNCTION_NAME", "mock-lambda");
        let json = read_json_file("lambda_function_url_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = LambdaFunctionUrlEvent::new(payload)
            .expect("Failed to deserialize LambdaFunctionUrlEvent");
        assert_eq!(
            event.get_arn("sa-east-1"),
            "arn:aws:lambda:sa-east-1:601427279990:url:mock-lambda"
        );
        env::remove_var("AWS_LAMBDA_FUNCTION_NAME");
    }
}