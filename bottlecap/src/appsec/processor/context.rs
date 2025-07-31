use std::collections::HashMap;
use std::io::{Read, read_to_string};
use std::time::Duration;

use bytes::{Buf, Bytes};
use datadog_trace_protobuf::pb::Span;
use libddwaf::object::{Keyed, WafMap, WafObject};
use libddwaf::{Context as WafContext, Handle, RunError, RunOutput, RunResult, waf_map};
use mime::Mime;
use multipart::server::Multipart;
use tracing::{debug, warn};

use crate::appsec::processor::InvocationPayload;
use crate::appsec::processor::response::ExpectedResponseFormat;

/// Holds inforamtion gathered about an invocation.
#[must_use]
pub struct Context {
    /// The request ID of the invocation.
    pub(super) rid: String,
    /// The expected response payload format
    pub(super) expected_response_format: ExpectedResponseFormat,
    /// The [`WafContext`] for this invocation.
    waf: WafContext,
    /// The timeout for the WAF.
    waf_timeout: Duration,
    // Whether this context has events.
    has_events: bool,
    /// Trace tags to be applied for this invocation.
    trace_tags: HashMap<TagName, TagValue>,
    /// Whether we have had a chance to see the response body or not.
    pub(super) response_seen: bool,
}
impl Context {
    /// Creates a new [`Context`] for the provided request ID and configures it
    /// with the provided WAF timeout.
    pub(crate) fn new(rid: String, waf_handle: &mut Handle, waf_timeout: Duration) -> Self {
        Self {
            rid,
            expected_response_format: ExpectedResponseFormat::default(),
            waf: waf_handle.new_context(),
            waf_timeout,
            has_events: false,
            trace_tags: HashMap::from([(TagName::AppsecEnabled, TagValue::Metric(1.0))]),
            response_seen: false,
        }
    }

    /// Evaluate the WAF rules against the provided [`InvocationPayload`] and
    /// collect the relevant side effects.
    pub(super) fn run(&mut self, payload: &dyn InvocationPayload) {
        if self.waf_timeout.is_zero() {
            warn!(
                "aap: WAF execution time budget for this request is exhausted, skipping WAF ruleset evaluation (consider tweaking DD_APPSEC_WAF_TIMEOUT)"
            );
            return;
        }

        // Update the expected response payload format if we haven't identified one yet.
        if self.expected_response_format.is_unknown() {
            self.expected_response_format = payload.corresponding_response_format();
        }

        // Extract span tag information from the payload.
        self.collect_span_tags(payload);

        // Extract address data from the payload.
        let Some(address_data) = to_address_data(payload) else {
            // Produced no address data, nothing more to do...
            return;
        };

        // Evaluate the WAF ruleset and handle the result.
        match self.waf.run(Some(address_data), None, self.waf_timeout) {
            Ok(RunResult::Match(result) | RunResult::NoMatch(result)) => {
                self.process_result(result);
            }
            Err(e) => log_waf_run_error(e),
        };
    }

    /// Obtain the information about the endpoint that was invoked, if available.
    pub(super) fn endpoint_info(&self) -> (String, String, String) {
        (
            self.trace_tags
                .get(&TagName::HttpMethod)
                .map(TagValue::to_string)
                .unwrap_or_default(),
            self.trace_tags
                .get(&TagName::HttpRoute)
                .map(TagValue::to_string)
                .unwrap_or_default(),
            self.trace_tags
                .get(&TagName::HttpStatusCode)
                .map(TagValue::to_string)
                .unwrap_or_default(),
        )
    }

    /// Enable API Security schema extraction for this context.
    pub(super) fn extract_schemas(&mut self) {
        let address_data = waf_map! {
            ("waf.context.processor", waf_map! {
                ("extract-schema", true)
            })
        };
        match self.waf.run(Some(address_data), None, self.waf_timeout) {
            Ok(RunResult::Match(result) | RunResult::NoMatch(result)) => {
                self.process_result(result);
            }
            Err(e) => log_waf_run_error(e),
        }
    }

    /// Add tags and metrics to a [`Span`].
    pub(super) fn process_span(&self, span: &mut Span) {
        for (key, value) in &self.trace_tags {
            match value {
                TagValue::Always(value) => {
                    span.meta.insert(key.to_string(), value.clone());
                }
                TagValue::OnEvent(value) => {
                    if !self.has_events {
                        continue;
                    }
                    match value {
                        serde_json::Value::String(value) => {
                            span.meta.insert(key.to_string(), value.clone());
                        }
                        value => {
                            span.meta.insert(key.to_string(), value.to_string());
                        }
                    }
                }
                TagValue::Metric(value) => {
                    span.metrics.insert(key.to_string(), *value);
                }
            }
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

    /// Collects the attributes from the provided [`InvocationPayload`] and
    /// keeps track of them on this [`Context`].
    fn collect_span_tags(&mut self, payload: &dyn InvocationPayload) {
        macro_rules! span_tag {
            // Simple tags
            ($field:ident on event => $name:expr) => {
                if let Some(value) = payload.$field() {
                    self.trace_tags.insert(
                        $name,
                        TagValue::OnEvent(serde_json::Value::String(value.clone())),
                    );
                }
            };
            ($field:ident => $name:expr) => {
                if let Some(value) = payload.$field() {
                    self.trace_tags
                        .insert($name, TagValue::Always(value.to_string()));
                }
            };
        }

        // Request span tags
        span_tag!(raw_uri => TagName::HttpUrl);
        span_tag!(method => TagName::HttpMethod);
        span_tag!(route on event => TagName::HttpRoute);
        span_tag!(client_ip on event => TagName::NetworkClientIp);
        // Response span tags
        span_tag!(response_status_code => TagName::HttpStatusCode);

        let headers = payload.request_headers_no_cookies();
        if !headers.is_empty() {
            for name in Self::REQUEST_HEADERS_ALWAYS {
                let Some(values) = headers.get(*name) else {
                    continue;
                };
                self.trace_tags.insert(
                    TagName::Dynamic(format!("http.request.headers.{name}")),
                    TagValue::Always(values.join(",")),
                );
            }
            for name in Self::REQUEST_HEADERS_ON_EVENT {
                let Some(values) = headers.get(*name) else {
                    continue;
                };
                self.trace_tags.insert(
                    TagName::Dynamic(format!("http.request.headers.{name}")),
                    TagValue::OnEvent(serde_json::Value::String(values.join(","))),
                );
            }
        }
        let headers = payload.response_headers_no_cookies();
        if !headers.is_empty() {
            for name in Self::RESPONSE_HEADERS_ALWAYS {
                let Some(values) = headers.get(*name) else {
                    continue;
                };
                self.trace_tags.insert(
                    TagName::Dynamic(format!("http.response.headers.{name}")),
                    TagValue::Always(values.join(",")),
                );
            }
        }
    }

    fn process_result(&mut self, result: RunOutput) {
        const SAMPLING_PRIORITY_USER_KEEP: f64 = 2.0;

        let duration = result.duration();
        self.waf_timeout = self.waf_timeout.saturating_sub(duration);
        debug!(
            "aap: WAF ruleset evaluation took {:?}, the remaining WAF budget is {:?}",
            duration, self.waf_timeout
        );
        if result.timeout() {
            warn!(
                "aap: time out reached while evaluating the WAF ruleset; detections may be incomplete. Consider tuning DD_APPSEC_WAF_TIMEOUT"
            );
        }

        if result.keep() {
            debug!("aap: a WAF rule requested the trace to be upgraded to USER_KEEP priority");
            self.trace_tags.insert(
                TagName::SamplingPriorityV1,
                TagValue::Metric(SAMPLING_PRIORITY_USER_KEEP),
            );
        }

        if let Some(attributes) = result.attributes() {
            for item in attributes.iter() {
                let name = match item.key_str() {
                    Ok(name) => name,
                    Err(e) => {
                        warn!(
                            "aap: falied to convert attribute name to string, the attribute will be ignored: {e}"
                        );
                        continue;
                    }
                };
                let value = if let Some(value) = item.to_str() {
                    value.to_string()
                } else {
                    match serde_json::to_string(item.inner()) {
                        Ok(value) => value,
                        Err(e) => {
                            warn!(
                                "aap: failed to convert attribute {name:#?} value to JSON, the attribute will be ignored: {e}\n{item:?}"
                            );
                            continue;
                        }
                    }
                };
                debug!("aap: produced synthetic tag {name}:{value}");
                self.trace_tags
                    .insert(TagName::Dynamic(name.to_string()), TagValue::Always(value));
            }
        }

        if let Some(events) = result.events() {
            debug!("aap: WAF ruleset detected {} events", events.len());
            if !events.is_empty() {
                self.trace_tags
                    .insert(TagName::AppsecEvent, TagValue::Always("true".to_string()));
                self.has_events = true;
            }
            let entry = self
                .trace_tags
                .entry(TagName::AppsecJson)
                .or_insert(TagValue::OnEvent(serde_json::Value::Array(Vec::new())));
            let TagValue::OnEvent(serde_json::Value::Array(entry)) = entry else {
                unreachable!(
                    "the {} tag entry is always a serde_json::Value::Array(...)",
                    TagName::AppsecJson
                );
            };
            entry.reserve(events.len());
            for event in events.iter() {
                let value = match serde_json::to_value(event) {
                    Ok(value) => value,
                    Err(e) => {
                        warn!(
                            "aap: failed to convert event to JSON, the event will be dropped: {e}\n{event:?}"
                        );
                        continue;
                    }
                };
                entry.push(value);
            }
        }

        if let Some(actions) = result.actions() {
            for action in actions.iter() {
                warn!(
                    "aap: WAF ruleset triggered actions that are currently ignored by Serverless AAP: {action:?}"
                );
            }
        }
    }
}

/// Names of tags that can be emitted by the WAF.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TagName {
    // AppSec tags
    AppsecEnabled,
    AppsecEvent,
    AppsecJson,
    // General request tags
    HttpUrl,
    HttpMethod,
    HttpRoute,
    HttpStatusCode,
    NetworkClientIp,
    // Special tags
    SamplingPriorityV1,
    /// A tag name that is not statically known.
    Dynamic(String),
}
impl TagName {
    fn as_str(&self) -> &str {
        match self {
            Self::AppsecEnabled => "_dd.appsec.enabled",
            Self::AppsecEvent => "appsec.event",
            Self::AppsecJson => "_dd.appsec.json",
            Self::HttpUrl => "http.url",
            Self::HttpMethod => "http.method",
            Self::HttpRoute => "http.route",
            Self::HttpStatusCode => "http.status_code",
            Self::NetworkClientIp => "network.client.ip",
            Self::SamplingPriorityV1 => "_sampling_priority_v1",
            Self::Dynamic(name) => name,
        }
    }
}
impl std::fmt::Display for TagName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
enum TagValue {
    /// A tag value that is always emitted.
    Always(String),
    /// A tag value that is only emitted when an event is matched.
    OnEvent(serde_json::Value),
    /// A tag value that actually is a metric value.
    Metric(f64),
}
impl std::fmt::Display for TagValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Always(str) => write!(f, "{str}"),
            Self::OnEvent(value) => write!(f, "{value}"),
            Self::Metric(value) => write!(f, "{value}"),
        }
    }
}

/// Produces a [`WafMap`] from the provided [`InvocationPayload`], returning
/// [`None`] if no address data is available.
fn to_address_data(payload: &dyn InvocationPayload) -> Option<WafMap> {
    let mut addresses = Vec::<Keyed<WafObject>>::with_capacity(10);

    macro_rules! address {
        // Option<String> kind of fields...
        ($field:ident$(.$transform:ident())? => $name:literal) => {
            if let Some(value) = payload.$field() {
                addresses.push(($name, value$(.$transform())?).into());
            }
        };

        // Bodies
        ($field:ident mime from $headers:ident => $name:literal) => {
            if let Some(body) = payload.$field() {
                let $headers = payload.$headers();
                // Note -- headers are case-normalized to lower case by the JSON parser...
                let content_type = $headers.get("content-type").map(|mime|mime.last()).flatten().map_or("application/json", |mime|mime.as_str());
                match try_parse_body(body, content_type) {
                    Ok(value) => {
                        addresses.push(($name, value).into());
                    },
                    Err(e) => warn!("aap: failed to parse body: {e}"),
                }
            }
        };

        // Single-valued HashMap fields...
        ($field:ident[] => $name:literal) => {
            let value = payload.$field();
            if !value.is_empty() {
                let mut waf_value = libddwaf::object::WafMap::new(value.len() as u64);
                for (idx, (key, item)) in value.iter().enumerate() {
                    waf_value[idx] = (key.as_str(), item.as_str()).into();
                }
                addresses.push(($name, waf_value).into());
            }
        };

        // Multi-valued HashMap fields...
        ($field:ident[][] => $name:literal) => {
            let value = payload.$field();
            if !value.is_empty() {
                let mut waf_value = libddwaf::object::WafMap::new(value.len() as u64);
                for (idx, (key, elements)) in value.iter().enumerate() {
                    let mut waf_elements = libddwaf::object::WafArray::new(elements.len() as u64);
                    for (idx, elt) in elements.iter().enumerate() {
                        waf_elements[idx] = elt.as_str().into();
                    }
                    waf_value[idx] = (key.as_str(), waf_elements).into();
                }
                addresses.push(($name, waf_value).into());
            }
        };
    }

    // Request addresses
    address!(raw_uri.as_str() => "server.request.uri.raw");
    address!(client_ip.as_str() => "http.client_ip");
    address!(request_headers_no_cookies[][] => "server.request.headers.no_cookies");
    address!(request_cookies[][] => "server.request.cookies");
    address!(query_params[][] => "server.request.query");
    address!(path_params[] => "server.request.path_params");
    address!(request_body mime from request_headers_no_cookies => "server.request.body");

    // Response addresses
    address!(response_status_code => "server.response.status");
    address!(response_headers_no_cookies[][] => "server.response.headers.no_cookies");
    address!(response_body mime from response_headers_no_cookies => "server.response.body");

    if addresses.is_empty() {
        return None;
    }

    let mut result = WafMap::new(addresses.len() as u64);
    for (idx, item) in addresses.into_iter().enumerate() {
        result[idx] = item;
    }

    Some(result)
}

fn try_parse_body(
    body: impl Read,
    content_type: &str,
) -> Result<WafObject, Box<dyn std::error::Error>> {
    let mime_type: Mime = content_type.parse()?;
    try_parse_body_with_mime(body, mime_type)
}

fn try_parse_body_with_mime(
    body: impl Read,
    mime_type: Mime,
) -> Result<WafObject, Box<dyn std::error::Error>> {
    match (mime_type.type_(), mime_type.subtype(), mime_type.suffix()) {
        (typ, sub, suff)
            if ((typ == mime::TEXT || typ == mime::APPLICATION)
                && sub == mime::JSON
                && suff.is_none())
                || (typ == mime::APPLICATION && sub == "vnd.api" && suff == Some(mime::JSON)) =>
        {
            Ok(serde_json::from_reader(body)?)
        }
        (mime::APPLICATION, mime::WWW_FORM_URLENCODED, None) => {
            let pairs: Vec<(String, String)> = serde_html_form::from_reader(body)?;
            let mut res = WafMap::new(pairs.len() as u64);
            for (i, (key, value)) in pairs.into_iter().enumerate() {
                res[i] = (key.as_str(), value.as_str()).into();
            }
            Ok(res.into())
        }
        (mime::MULTIPART, mime::FORM_DATA, None) => {
            let Some(boundary) = mime_type.get_param("boundary") else {
                return Err(format!(
                    "aap: cannot parse {mime_type} body: missing boundary parameter"
                )
                .into());
            };
            let mut multipart = Multipart::with_body(body, boundary.as_str());
            let mut items = Vec::new();
            loop {
                match multipart.read_entry() {
                    Ok(Some(mut entry)) => {
                        let mime = entry.headers.content_type.unwrap_or(mime::TEXT_PLAIN);
                        let mut data = Vec::new();
                        let _ = entry.data.read_to_end(&mut data)?;
                        let value = match try_parse_body_with_mime(Bytes::from(data).reader(), mime)
                        {
                            Ok(value) => value,
                            Err(e) => {
                                warn!(
                                    "aap: failed to parse multipart body entry {name}: {e}",
                                    name = entry.headers.name
                                );
                                WafObject::default()
                            }
                        };

                        items.push((entry.headers.name.to_string(), value));
                    }
                    Ok(None) => break,
                    Err(e) => {
                        warn!("aap: failed to read multipart body entry: {e}");
                        break;
                    }
                }
            }
            let mut res = WafMap::new(items.len() as u64);
            for (idx, (name, value)) in items.into_iter().enumerate() {
                res[idx] = (name.as_str(), value).into();
            }
            Ok(res.into())
        }
        (mime::TEXT, mime::PLAIN, None) => {
            let body = read_to_string(body)?;
            Ok(body.as_str().into())
        }
        _ => Err(
            format!("aap: unsupported MIME type, the body will not be parsed: {mime_type}").into(),
        ),
    }
}

#[cold]
fn log_waf_run_error(e: RunError) {
    warn!("aap: failed to evaluate WAF ruleset: {e}");
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
mod tests {
    use std::io;

    use libddwaf::waf_map;

    use super::*;

    #[test]
    fn test_try_parse_body() {
        let body = r#"--BOUNDARY
Content-Disposition: form-data; name="json"
Content-Type: application/json

{"foo": "bar"}
--BOUNDARY
Content-Disposition: form-data; name="text"

Hello, world!
--BOUNDARY
Content-Disposition: form-data; name="vnd.api+json"
Content-Type: application/vnd.api+json; charset=utf-8

{"baz": "qux"}

--BOUNDARY
Content-Disposition: form-data; name="invalid"
Content-Type: text/json

{invalid json}
--BOUNDARY
Content-Disposition: form-data; name="urlencoded"
Content-Type: application/x-www-form-urlencoded

key=value&space=%20
--BOUNDARY--"#
            .replace('\n', "\r\n"); // LF -> CRLF
        let body = Bytes::from(body);
        let parsed = try_parse_body(body.reader(), "multipart/form-data; boundary=BOUNDARY")
            .expect("should have parsed successfully");
        assert_eq!(
            waf_map! {
                ("json", waf_map!{ ("foo", "bar") }),
                ("text", "Hello, world!"),
                ("vnd.api+json", waf_map!{ ("baz", "qux") }),
                ("invalid", WafObject::default()),
                ("urlencoded", waf_map!{ ("key", "value"), ("space", " ") }),
            },
            parsed,
        );
    }

    #[test]
    fn test_try_parse_body_missing_boundary() {
        try_parse_body(io::empty(), "multipart/form-data").expect_err("should have failed");
    }
}
