use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::appsec::payload::ToWafMap;
use crate::appsec::{is_enabled, is_standalone, payload};
use crate::config::Config;

use bytes::Bytes;
use libddwaf::object::{WafMap, WafOwned};
use libddwaf::{Builder, Config as WAFConfig, Context, Handle, RunResult};
use tracing::{debug, info, warn};
/// The App & API Protection processor.
///
/// It is used to try to identify invoke requests that are supported, extract the relevant data from
/// the request payload, and evaluate in-app WAF rules against that data.
pub struct Processor {
    handle: Handle,
    waf_timeout: Duration,
}
impl Processor {
    /// Creates a new [`Processor`] instance using the provided [`Config`].
    ///
    /// # Errors
    /// - If [`Config::serverless_appsec_enabled`] is `false`;
    /// - If the [`Config::appsec_rules`] points to a non-existent file;
    /// - If the [`Config::appsec_rules`] points to a file that is not a valid JSON-encoded ruleset;
    /// - If the in-app WAF fails to initialize, integrate the ruleset, or build the WAF instance.
    pub fn new(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        if !is_enabled(config) {
            return Err("AppSec is not enabled".into());
        }
        debug!("Starting ASM processor");

        if is_standalone() {
            info!(
                "Starting ASM in standalone mode. APM tracing will be disabled for this service."
            );
        }

        let Some(mut builder) = Builder::new(&WAFConfig::default()) else {
            return Err("Failed to create WAF builder".into());
        };

        let rules = Self::get_rules(config)?;
        let mut diagnostics = WafOwned::<WafMap>::default();
        if !builder.add_or_update_config("rules", &rules, Some(&mut diagnostics)) {
            return Err("Failed to add ruleset to the WAF builder".into());
        }

        let Some(handle) = builder.build() else {
            return Err("Failed to build WAF instance".into());
        };

        if let Some(version) = diagnostics.get(b"ruleset_version").and_then(|o| o.to_str()) {
            debug!("appsec: loaded ruleset vesion: {version}");
        }

        Ok(Self {
            handle,
            waf_timeout: config.appsec_waf_timeout,
        })
    }

    /// Process the `/runtime/invocation/next` payload, which is sent to Lambda to request an
    /// invocation event.
    #[must_use]
    pub async fn process_invocation_next(&self, body: &Bytes) -> Option<AppSecContext> {
        let (address_data, request_type) = payload::extract_request_address_data(body).await?;

        let mut context = AppSecContext {
            request_type,
            waf_context: Arc::new(Mutex::new(self.handle.new_context())),
            waf_timeout: self.waf_timeout,
            duration: Duration::ZERO,
            timeouts: 0,
            keep: false,
            attributes: HashMap::new(),
            tags_always: HashMap::new(),
            tags_on_event: HashMap::new(),
            events: Vec::new(),
        };

        context
            .tags_always
            .insert("_dd.origin".to_string(), "appsec".to_string());

        context.absorb_data(address_data);
        Some(context)
    }

    /// Process the `/runtime/invocation/<request_id>/response>` payload, which is sent to Lambda
    /// after the invocation has run to completion, to provide the result of the invocation.
    pub async fn process_invocation_response(&self, context: &mut AppSecContext, body: &Bytes) {
        let Some(address_data) =
            payload::extract_response_address_data(context.request_type, body).await
        else {
            return;
        };

        context.absorb_data(address_data);
    }

    /// Parses the App & API Protection ruleset from the provided [Config], falling back to the
    /// default built-in ruleset if the [Config] has [None].
    fn get_rules(config: &Config) -> Result<WafMap, Box<dyn std::error::Error>> {
        // Default on recommended rules
        match &config.appsec_rules {
            None => {
                let default_rules = include_bytes!("rules.json");
                Ok(serde_json::from_slice(default_rules)?)
            }
            Some(path) => {
                let rules = std::fs::File::open(path)?;
                Ok(serde_json::from_reader(rules)?)
            }
        }
    }
}
impl std::fmt::Debug for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(Processor))
            .field("waf_timeout", &self.waf_timeout)
            .finish_non_exhaustive()
    }
}

/// Request headers that are always collected as long as AAP is enabled.
///
/// This list should contain lowercase-normalized header names.
///
/// See: <https://datadoghq.atlassian.net/wiki/spaces/SAAL/pages/2186870984/HTTP+header+collection>.
const REQUEST_HEADERS_ON_EVENT: &[&str] = &[
    // IP address releated headers
    "x-forwarded-for",
    "x-real-ip",
    "true-client-ip",
    "x-client-ip",
    "x-forwarded",
    "forwarded-for",
    "x-cluster-client-ip",
    "fastly-client-ip",
    "cf-connecting-ip",
    "cf-connecting-ipv6",
    "forwarded",
    "via",
    // Message body information
    "content-length",
    "content-encoding",
    "content-language",
    // Host request context
    "host",
    // Content negotiation
    "accept-encoding",
    "accept-language",
];
/// Request headers that are collected only if AAP is enabled, and security activity has been
/// detected.
///
/// This list should contain lowercase-normalized header names.
///
/// See: <https://datadoghq.atlassian.net/wiki/spaces/SAAL/pages/2186870984/HTTP+header+collection>.
const REQUEST_HEADERS_ALWAYS: &[&str] = &[
    // Message body information
    "content-type",
    // Client user agent
    "user-agent",
    // Content negotiation
    "accept",
    // AWS WAF logs to traces (RFC 0996)
    "x-amzn-trace-id",
    // WAF Integration - Identify Requests (RFC 0992)
    "cloudfront-viewer-ja3-fingerprint",
    "cf-ray",
    "x-cloud-trace-context",
    "x-appgw-trace-id",
    "x-sigsci-requestid",
    "x-sigsci-tags",
    "akamai-user-risk",
];
/// Response headers that are always collected as long as AAP is enabled.
///
/// See: <https://datadoghq.atlassian.net/wiki/spaces/SAAL/pages/2186870984/HTTP+header+collection>.
const RESPONSE_HEADERS_ALWAYS: &[&str] = &[
    // Message body information
    "content-length",
    "content-type",
    "content-encoding",
    "content-language",
];

/// The WAF context for a single invocation.
///
/// This is used to process both the request & response of a given invocation.
#[derive(Clone)]
pub struct AppSecContext {
    request_type: payload::RequestType,
    // This must be clone-able due to how the request contexts are handled, so we have to wrap the
    // WAF context in an Arc-Mutex.
    waf_context: Arc<Mutex<Context>>,
    waf_timeout: Duration,
    pub duration: Duration,
    pub timeouts: u32,
    pub keep: bool,
    attributes: HashMap<String, String>,
    /// The trace tags that are added to the trace unconditionally.
    tags_always: HashMap<String, String>,
    /// The trace tags that are added to the trace ONLY if there is a security event.
    tags_on_event: HashMap<String, String>,
    pub events: Vec<String>,
}
impl AppSecContext {
    /// Returns the list of trace tags to add to the trace.
    pub fn tags(&self) -> impl Iterator<Item = (&String, &String)> {
        let next: Box<dyn Iterator<Item = (&String, &String)>> = if self.events.is_empty() {
            Box::new(std::iter::empty())
        } else {
            Box::new(self.tags_on_event.iter())
        };
        self.attributes
            .iter()
            .chain(self.tags_always.iter())
            .chain(next)
    }

    /// Evaluates the appsec rules against the provided request data, and creates any relevant
    /// attributes from it.
    fn absorb_data(&mut self, address_data: payload::HttpData) {
        match &address_data {
            payload::HttpData::Request {
                raw_uri,
                method,
                route,
                client_ip,
                headers,
                ..
            } => {
                if let Some(uri) = raw_uri {
                    self.tags_always
                        .entry("http.url".to_string())
                        .or_insert(uri.clone());
                }
                if let Some(method) = method {
                    self.tags_always
                        .entry("http.method".to_string())
                        .or_insert(method.clone());
                }
                if let Some(route) = route {
                    self.tags_on_event
                        .entry("http.endpoint".to_string())
                        .or_insert(route.clone());
                }
                if let Some(headers) = headers {
                    for name in REQUEST_HEADERS_ALWAYS {
                        let Some(values) = headers.get(*name) else {
                            continue;
                        };
                        self.tags_always
                            .entry(format!("http.request.headers.{name}"))
                            .or_insert(values.join(","));
                    }
                    for name in REQUEST_HEADERS_ON_EVENT {
                        let Some(values) = headers.get(*name) else {
                            continue;
                        };
                        self.tags_on_event
                            .entry(format!("http.request.headers.{name}"))
                            .or_insert(values.join(","));
                    }
                }
                if let Some(client_ip) = client_ip {
                    self.tags_on_event
                        .entry("network.client.ip".to_string())
                        .or_insert(client_ip.clone());
                }
            }
            payload::HttpData::Response {
                status_code,
                headers,
                ..
            } => {
                if let Some(status_code) = status_code {
                    self.tags_always
                        .entry("http.status_code".to_string())
                        .or_insert(status_code.to_string());
                }
                if let Some(headers) = headers {
                    for name in RESPONSE_HEADERS_ALWAYS {
                        let Some(values) = headers.get(*name) else {
                            continue;
                        };
                        self.tags_always
                            .entry(format!("http.response.headers.{name}"))
                            .or_insert(values.join(","));
                    }
                }
            }
        }

        self.run(address_data.to_waf_map());
    }

    /// Evaluates the in-app WAF rules against the provided address data.
    fn run(&mut self, address_data: WafMap) {
        let timeout = self.waf_timeout.saturating_sub(self.duration);
        if timeout == Duration::ZERO {
            warn!(
                "appsec: WAF timeout already reached, not evaluating request with {address_data:?}"
            );
            return;
        }

        let mut waf_context = match self.waf_context.lock() {
            Ok(waf_context) => waf_context,
            Err(e) => {
                warn!("appsec: failed to lock WAF context: {e}");
                return;
            }
        };
        let result = match waf_context.run(Some(address_data), None, timeout) {
            Ok(RunResult::Match(result) | RunResult::NoMatch(result)) => result,
            Err(e) => {
                warn!("Failed to evalute in-app WAF rules against request: {e}");
                return;
            }
        };

        self.duration += result.duration();
        if result.timeout() {
            self.timeouts += 1;
        }
        if result.keep() {
            self.keep = true;
        }
        if let Some(attributes) = result.attributes() {
            self.attributes.reserve(attributes.len());
            for attr in attributes.iter() {
                let Ok(key) = attr.key_str() else { continue };
                let value = if let Some(value) = attr.to_str() {
                    value.to_string()
                } else if let Some(value) = attr.to_u64() {
                    value.to_string()
                } else if let Some(value) = attr.to_i64() {
                    value.to_string()
                } else if let Some(value) = attr.to_f64() {
                    value.to_string()
                } else if let Some(value) = attr.to_bool() {
                    value.to_string()
                } else {
                    debug!("appsec: unsupported attribute produced by the WAF: {attr:?}");
                    continue;
                };
                self.attributes.insert(key.to_string(), value);
            }
        }
        if let Some(events) = result.events() {
            self.events.reserve(events.len());
            for event in events.iter() {
                let enc = match serde_json::to_string(event) {
                    Ok(enc) => enc,
                    Err(e) => {
                        warn!("appsec: unable to encode WAF event: {e}\n{event:?}");
                        continue;
                    }
                };
                self.events.push(enc);
            }
        }
    }
}
impl std::fmt::Debug for AppSecContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(AppSecContext))
            .field("request_type", &self.request_type)
            .field("waf_timeout", &self.waf_timeout)
            .field("duration", &self.duration)
            .field("timeouts", &self.timeouts)
            .field("keep", &self.keep)
            .field("attributes", &self.attributes)
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}
impl std::cmp::PartialEq for AppSecContext {
    fn eq(&self, other: &Self) -> bool {
        self.request_type == other.request_type
            && self.waf_timeout == other.waf_timeout
            && self.duration == other.duration
            && self.timeouts == other.timeouts
            && self.keep == other.keep
            && self.attributes == other.attributes
            && self.events == other.events
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    use super::*;

    #[test]
    fn test_new_with_default_config() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let _ = Processor::new(&config).expect("Should not fail");
    }

    #[test]
    fn test_new_with_invalid_config() {
        let tmp = tempfile::NamedTempFile::new().expect("Failed to create tempfile");

        let config = Config {
            serverless_appsec_enabled: true,
            appsec_rules: Some(
                tmp.path()
                    .to_str()
                    .expect("Failed to get tempfile path")
                    .to_string(),
            ),
            ..Config::default()
        };
        let _ = Processor::new(&config).expect_err("should have failed");
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_api_gateway_v1() {
        let config = Config {
            serverless_appsec_enabled: true,
            appsec_waf_timeout: Duration::from_secs(3600), // Avoids falkes on slower CI hardware
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "resource": "/{proxy+}",
            "path": "/path/to/resource",
            "httpMethod": "POST",
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json"
            },
            "multiValueHeaders": {
                "Accept": ["*/*"],
                "Content-Type": ["application/json"]
            },
            "queryStringParameters": {
                "foo": "bar"
            },
            "multiValueQueryStringParameters": {
                "foo": ["bar"]
            },
            "pathParameters": {
                "proxy": "/path/to/resource"
            },
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/{proxy+}",
                "httpMethod": "POST",
                "extendedRequestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "requestTime": "09/Apr/2015:12:34:56 +0000",
                "path": "/path/to/resource",
                "accountId": "123456789012",
                "protocol": "HTTP/1.1",
                "stage": "prod",
                "domainPrefix": "1234567890",
                "requestTimeEpoch": 1428582896000,
                "requestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "identity": {
                    "cognitoIdentityPoolId": null,
                    "accountId": null,
                    "cognitoIdentityId": null,
                    "caller": null,
                    "accessKey": null,
                    "sourceIp": "127.0.0.1",
                    "cognitoAuthenticationType": null,
                    "cognitoAuthenticationProvider": null,
                    "userArn": null,
                    "userAgent": "Custom User Agent String",
                    "user": null
                },
                "domainName": "1234567890.execute-api.us-east-1.amazonaws.com",
                "apiId": "1234567890"
            },
            "body": "{\"test\":\"body\"}",
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        let context = context.expect("an AppSec context should have been created");
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);
        assert!(context.tags_always.contains_key("_dd.origin"));
        assert_eq!(
            context.tags_always.get("_dd.origin"),
            Some(&"appsec".to_string())
        );
        // Duration might be greater than zero due to WAF processing
        assert!(context.duration >= Duration::ZERO);
        assert_eq!(context.timeouts, 0);
        assert!(!context.keep);
        assert!(context.events.is_empty());
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_api_gateway_v2() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "version": "2.0",
            "routeKey": "GET /httpapi/get",
            "rawPath": "/httpapi/get",
            "rawQueryString": "foo=bar",
            "cookies": ["cookie1", "cookie2"],
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json",
                "Host": "example.amazonaws.com"
            },
            "queryStringParameters": {
                "foo": "bar"
            },
            "requestContext": {
                "accountId": "123456789012",
                "apiId": "1234567890",
                "authentication": {
                    "clientCert": {
                        "clientCertPem": "CERT_CONTENT",
                        "subjectDN": "www.example.com",
                        "issuerDN": "Example issuer",
                        "serialNumber": "a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1",
                        "validity": {
                            "start": "May 28 12:30:02 2019 GMT",
                            "end": "Aug  5 09:36:04 2021 GMT"
                        }
                    }
                },
                "domainName": "example.amazonaws.com",
                "domainPrefix": "1234567890",
                "http": {
                    "method": "GET",
                    "path": "/httpapi/get",
                    "protocol": "HTTP/1.1",
                    "sourceIp": "192.168.1.1",
                    "userAgent": "agent"
                },
                "requestId": "JKJaXmPLvHcESHA=",
                "routeKey": "GET /httpapi/get",
                "stage": "$default",
                "time": "10/Mar/2020:05:28:40 +0000",
                "timeEpoch": 1583817320220
            },
            "body": "{\"message\":\"hello world\"}",
            "pathParameters": {
                "parameter1": "value1"
            },
            "isBase64Encoded": false,
            "stageVariables": {
                "stageVariable1": "value1",
                "stageVariable2": "value2"
            }
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        let context = context.expect("an AppSec context should have been created");
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV2Http);
        assert!(context.tags_always.contains_key("_dd.origin"));
        assert_eq!(
            context.tags_always.get("_dd.origin"),
            Some(&"appsec".to_string())
        );
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_unsupported_payload() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "Records": [
                {
                    "EventSource": "aws:sns",
                    "EventVersion": "1.0",
                    "EventSubscriptionArn": "arn:aws:sns:us-east-1:123456789012:example-topic:2bcfbf39-05c3-41de-beaa-fcfcc21c8f55",
                    "Sns": {
                        "Type": "Notification",
                        "MessageId": "95df01b4-ee98-5cb9-9903-4c221d41eb5e",
                        "TopicArn": "arn:aws:sns:us-east-1:123456789012:example-topic",
                        "Subject": "example subject",
                        "Message": "example message",
                        "Timestamp": "1970-01-01T00:00:00.000Z",
                        "SignatureVersion": "1",
                        "Signature": "EXAMPLE",
                        "SigningCertUrl": "EXAMPLE",
                        "UnsubscribeUrl": "EXAMPLE"
                    }
                }
            ]
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        // SNS events are not supported, so should return None
        assert!(context.is_none());
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_invalid_json() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{"invalid": json}"#;

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        // Invalid JSON should return None
        assert!(context.is_none());
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_empty_payload() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = "";

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        // Empty payload should return None
        assert!(context.is_none());
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_api_gateway_v1() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let request_payload = r#"{
            "resource": "/{proxy+}",
            "path": "/path/to/resource",
            "httpMethod": "POST",
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json"
            },
            "multiValueHeaders": {
                "Accept": ["*/*"],
                "Content-Type": ["application/json"]
            },
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/{proxy+}",
                "httpMethod": "POST",
                "stage": "prod",
                "identity": {
                    "sourceIp": "127.0.0.1"
                }
            },
            "body": "{\"test\":\"body\"}",
            "isBase64Encoded": false
        }"#;

        let request_bytes = Bytes::from(request_payload);
        let mut context = processor
            .process_invocation_next(&request_bytes)
            .await
            .expect("Should create context");

        // Now test the response processing
        let response_payload = r#"{
            "statusCode": 200,
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "{\"response\":\"success\"}",
            "isBase64Encoded": false
        }"#;

        let response_bytes = Bytes::from(response_payload);

        // This should not panic and should complete successfully
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state after response processing
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_api_gateway_v2() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let request_payload = r#"{
            "version": "2.0",
            "routeKey": "GET /httpapi/get",
            "rawPath": "/httpapi/get",
            "rawQueryString": "foo=bar",
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json",
                "Host": "example.amazonaws.com"
            },
            "requestContext": {
                "accountId": "123456789012",
                "apiId": "1234567890",
                "domainName": "example.amazonaws.com",
                "domainPrefix": "1234567890",
                "http": {
                    "method": "GET",
                    "path": "/httpapi/get",
                    "protocol": "HTTP/1.1",
                    "sourceIp": "192.168.1.1",
                    "userAgent": "agent"
                },
                "requestId": "JKJaXmPLvHcESHA=",
                "routeKey": "GET /httpapi/get",
                "stage": "$default",
                "time": "10/Mar/2020:05:28:40 +0000",
                "timeEpoch": 1583817320220
            },
            "body": "{\"message\":\"hello world\"}",
            "isBase64Encoded": false
        }"#;

        let request_bytes = Bytes::from(request_payload);
        let mut context = processor
            .process_invocation_next(&request_bytes)
            .await
            .expect("Should create context");

        // Now test the response processing
        let response_payload = r#"{
            "statusCode": 200,
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "{\"response\":\"success\"}",
            "isBase64Encoded": false
        }"#;

        let response_bytes = Bytes::from(response_payload);

        // This should not panic and should complete successfully
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state after response processing
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV2Http);
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_invalid_response() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let request_payload = r#"{
            "resource": "/{proxy+}",
            "path": "/path/to/resource",
            "httpMethod": "POST",
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json"
            },
            "multiValueHeaders": {
                "Accept": ["*/*"],
                "Content-Type": ["application/json"]
            },
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/{proxy+}",
                "httpMethod": "POST",
                "stage": "prod",
                "identity": {
                    "sourceIp": "127.0.0.1"
                }
            },
            "body": "{\"test\":\"body\"}",
            "isBase64Encoded": false
        }"#;

        let request_bytes = Bytes::from(request_payload);
        let mut context = processor
            .process_invocation_next(&request_bytes)
            .await
            .expect("Should create context");

        // Now test the response processing with invalid JSON
        let response_payload = r#"{"invalid": json}"#;

        let response_bytes = Bytes::from(response_payload);

        // This should not panic even with invalid response JSON
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state is still valid
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_empty_response() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let request_payload = r#"{
            "resource": "/{proxy+}",
            "path": "/path/to/resource",
            "httpMethod": "POST",
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json"
            },
            "multiValueHeaders": {
                "Accept": ["*/*"],
                "Content-Type": ["application/json"]
            },
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/{proxy+}",
                "httpMethod": "POST",
                "stage": "prod",
                "identity": {
                    "sourceIp": "127.0.0.1"
                }
            },
            "body": "{\"test\":\"body\"}",
            "isBase64Encoded": false
        }"#;

        let request_bytes = Bytes::from(request_payload);
        let mut context = processor
            .process_invocation_next(&request_bytes)
            .await
            .expect("Should create context");

        // Now test the response processing with empty response
        let response_payload = "";

        let response_bytes = Bytes::from(response_payload);

        // This should not panic even with empty response
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state is still valid
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);
    }

    #[tokio::test]
    async fn test_appsec_context_absorb_data_does_not_panic() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // Create a context with various payload types to ensure absorb_data doesn't panic
        let test_payloads = vec![
            // API Gateway V1
            r#"{
                "resource": "/{proxy+}",
                "path": "/path/to/resource",
                "httpMethod": "POST",
                "headers": {
                    "Content-Type": "application/json"
                },
                "multiValueHeaders": {
                    "Content-Type": ["application/json"]
                },
                "requestContext": {
                    "resourceId": "123456",
                    "resourcePath": "/{proxy+}",
                    "httpMethod": "POST",
                    "stage": "prod",
                    "identity": {
                        "sourceIp": "127.0.0.1"
                    }
                },
                "body": "{\"test\":\"body\"}",
                "isBase64Encoded": false
            }"#,
            // Lambda Function URL
            r#"{
                "version": "2.0",
                "routeKey": "$default",
                "rawPath": "/my/path",
                "rawQueryString": "parameter1=value1",
                "headers": {
                    "Header1": "value1",
                    "Content-Type": "application/json"
                },
                "requestContext": {
                    "accountId": "123456789012",
                    "apiId": "r3pmxmplak",
                    "domainName": "r3pmxmplak.lambda-url.us-east-2.on.aws",
                    "domainPrefix": "r3pmxmplak",
                    "http": {
                        "method": "POST",
                        "path": "/my/path",
                        "protocol": "HTTP/1.1",
                        "sourceIp": "123.123.123.123",
                        "userAgent": "agent"
                    },
                    "requestId": "id",
                    "routeKey": "$default",
                    "stage": "$default",
                    "time": "12/Mar/2020:19:03:58 +0000",
                    "timeEpoch": 1584043438390
                },
                "body": "Hello from Lambda!",
                "isBase64Encoded": false
            }"#,
        ];

        for payload in test_payloads {
            let bytes = Bytes::from(payload);
            let context = processor.process_invocation_next(&bytes).await;

            // Should not panic and should create a valid context
            assert!(context.is_some());
            let context = context.unwrap();
            assert!(context.tags_always.contains_key("_dd.origin"));
        }
    }
}
