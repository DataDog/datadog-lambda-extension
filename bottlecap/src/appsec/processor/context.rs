use std::collections::HashMap;
use std::io::{Read, read_to_string};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, Bytes};
use libdd_trace_protobuf::pb::Span;
use libdd_trace_utils::tracer_header_tags;
use libddwaf::object::{Keyed, WafMap, WafObject};
use libddwaf::{Context as WafContext, Handle, RunError, RunOutput, RunResult, waf_map};
use mime::Mime;
use multipart::server::Multipart;
use tracing::{debug, info, warn};

use crate::appsec::processor::response::ExpectedResponseFormat;
use crate::appsec::processor::{InvocationPayload, Processor};
use crate::config::Config;
use crate::tags::provider::Provider;
use crate::traces::span_pointers::SpanPointer;
use crate::traces::trace_processor::SendingTraceProcessor;

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
    /// The total time spent on WAF runs.
    waf_duration: Duration,
    /// The number of times the WAF timed out during this request.
    waf_timed_out_occurrences: usize,
    // Whether this context has events.
    has_events: bool,
    /// Trace tags to be applied for this invocation.
    trace_tags: HashMap<TagName, TagValue>,
    /// Whether we have had a chance to see the response body or not.
    response_seen: bool,
    /// Holds a trace for sending once the response is seen.
    held_trace: Option<(Vec<Span>, SendingTraceProcessor, HoldArguments)>,
}
impl Context {
    /// Creates a new [`Context`] for the provided request ID and configures it
    /// with the provided WAF timeout.
    pub(crate) fn new(
        rid: String,
        waf_handle: &mut Handle,
        ruleset_version: &str,
        waf_timeout: Duration,
    ) -> Self {
        let mut trace_tags = HashMap::from([
            (TagName::AppsecEnabled, TagValue::Metric(1.0)),
            (
                TagName::AppsecWafVersion,
                TagValue::Always(libddwaf::get_version().to_string_lossy().to_string()),
            ),
        ]);
        if !ruleset_version.is_empty() {
            trace_tags.insert(
                TagName::AppsecEventRulesVersion,
                TagValue::Always(ruleset_version.to_string()),
            );
        }

        Self {
            rid,
            expected_response_format: ExpectedResponseFormat::default(),
            waf: waf_handle.new_context(),
            waf_timeout,
            waf_duration: Duration::ZERO,
            waf_timed_out_occurrences: 0,
            has_events: false,
            trace_tags,
            response_seen: false,
            held_trace: None,
        }
    }

    /// Returns [`true`] if the response for this request was not seen yet and
    /// it might still be occurring.
    pub(crate) const fn is_pending_response(&self) -> bool {
        !self.response_seen
    }

    /// Marks the response of this request as having been seen.
    ///
    /// This implies the receiving [`Context`] has collected all possible
    /// security information about the corresponding request, and [`Span`]s sent
    /// through [`Self::process_span`] can be considered finalized and ready to
    /// flush to the trace aggregator.
    pub(super) async fn set_response_seen(&mut self) {
        if self.response_seen {
            return;
        }

        if let Some((mut trace, sender, args)) = self.held_trace.take() {
            if let Some(span) = Processor::service_entry_span_mut(&mut trace) {
                // Debug-sanity check that we're holding the right trace.
                debug_assert_eq!(
                    span.meta.get("request_id").map(String::as_str),
                    Some(self.rid.as_str())
                );
                debug!(
                    "aap: processing trace for request {} now that the response was seen",
                    self.rid
                );
                self.process_span(span);
            }

            debug!("aap: flushing out trace for request {}", self.rid);
            match sender
                .send_processed_traces(
                    args.config,
                    args.tags_provider,
                    tracer_header_tags::TracerHeaderTags {
                        lang: &args.tracer_header_tags_lang,
                        lang_version: &args.tracer_header_tags_lang_version,
                        lang_interpreter: &args.tracer_header_tags_lang_interpreter,
                        lang_vendor: &args.tracer_header_tags_lang_vendor,
                        tracer_version: &args.tracer_header_tags_tracer_version,
                        container_id: &args.tracer_header_tags_container_id,
                        client_computed_top_level: args
                            .tracer_header_tags_client_computed_top_level,
                        client_computed_stats: args.tracer_header_tags_client_computed_stats,
                        dropped_p0_traces: args.tracer_header_tags_dropped_p0_traces,
                        dropped_p0_spans: args.tracer_header_tags_dropped_p0_spans,
                    },
                    vec![trace],
                    args.body_size,
                    args.span_pointers,
                )
                .await
            {
                Ok(()) => debug!("aap: successfully sent trace to aggregator buffer"),
                Err(e) => warn!("aap: failed to send trace to aggregator buffer: {e}"),
            }
        }

        debug!(
            "aap: marking security context for {} as having seen the response...",
            self.rid
        );
        self.response_seen = true;
    }

    /// Holds a trace for future processing once the response is seen.
    pub(crate) fn hold_trace(
        &mut self,
        trace: Vec<Span>,
        sender: SendingTraceProcessor,
        args: HoldArguments,
    ) {
        if !self.is_pending_response() {
            unreachable_warn("Context::hold_trace called after response was seen!");
            return;
        }
        self.held_trace = Some((trace, sender, args));
    }

    /// Evaluate the WAF rules against the provided [`InvocationPayload`] and
    /// collect the relevant side effects.
    pub(super) fn run(&mut self, payload: &dyn InvocationPayload) {
        if self.response_seen {
            unreachable_warn("aap: Context::run called after response was seen!");
        }

        if self.waf_timeout.is_zero() {
            // Logging as INFO and not as WARN as users may find this an acceptable trade-off in
            // situations where extending the WAF timeout is not acceptable. Issuing a WARN here
            // would lead to excessive log spam for these customers, who then can't do anything
            // about it.
            info!(
                "aap: WAF execution time budget for this request is exhausted, skipping WAF ruleset evaluation entirely (consider tweaking DD_APPSEC_WAF_TIMEOUT)"
            );
            // This still counts as a WAF timeout even though we did not actually call it... The
            // budget would have been 0 here.
            self.waf_timed_out_occurrences += 1;
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
        }
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
        debug!(
            "aap: setting up to {} span tags/metrics on span {}",
            self.trace_tags.len(),
            span.span_id
        );
        for (key, value) in &self.trace_tags {
            match value {
                TagValue::Always(value) => {
                    span.meta.insert(key.to_string(), value.clone());
                }
                TagValue::AppSecEvents { triggers } => {
                    span.meta.insert(
                        key.to_string(),
                        serde_json::Value::Object(serde_json::Map::from_iter([(
                            "triggers".to_string(),
                            serde_json::Value::Array(triggers.clone()),
                        )]))
                        .to_string(),
                    );
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

        span.metrics.insert(
            TagName::AppsecWafDuration.to_string(),
            self.waf_duration.as_micros() as f64,
        );
        span.metrics.insert(
            TagName::AppsecWafTimeouts.to_string(),
            self.waf_timed_out_occurrences as f64,
        );
        #[allow(clippy::map_entry)] // We want to emit a debug log here...
        if !span.meta.contains_key(&TagName::Origin.to_string()) {
            debug!("aap: setting span tag {}:appsec", TagName::Origin);
            span.meta
                .insert(TagName::Origin.to_string(), "appsec".to_string());
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
        self.waf_duration += duration;
        debug!(
            "aap: WAF ruleset evaluation took {:?}, the remaining WAF budget is {:?} (total time spent so far: {:?})",
            duration, self.waf_timeout, self.waf_duration
        );
        if result.timeout() {
            // Logging as INFO and not as WARN as users may find this an acceptable trade-off in
            // situations where extending the WAF timeout is not acceptable. Issuing a WARN here
            // would lead to excessive log spam for these customers, who then can't do anything
            // about it.
            info!(
                "aap: time out reached while evaluating the WAF ruleset; detections may be incomplete. Consider tuning DD_APPSEC_WAF_TIMEOUT"
            );
            self.waf_timed_out_occurrences += 1;
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
                let entry =
                    self.trace_tags
                        .entry(TagName::AppsecJson)
                        .or_insert(TagValue::AppSecEvents {
                            triggers: Vec::with_capacity(events.len()),
                        });
                let TagValue::AppSecEvents { triggers } = entry else {
                    unreachable!(
                        "the {} tag entry is always a TagValue::AppSecEvents{{...}}",
                        TagName::AppsecJson
                    );
                };
                triggers.reserve(events.len());
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
                    triggers.push(value);
                }
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
impl Drop for Context {
    fn drop(&mut self) {
        // First off, gentle log nudge -- we should have marked the context's response as seen before this can drop...
        if !self.response_seen {
            debug!(
                "aap: Context being dropped without the response being marked as seen, this may cause traces to be dropped"
            );
        }
        // In debug assertions mode, hard-crash if it means we're effectively dropping a trace.
        debug_assert!(
            self.response_seen || self.held_trace.is_none(),
            "aap: Context is being dropped without the response being marked as seen. A trace will be dropped!"
        );
    }
}

pub struct HoldArguments {
    pub config: Arc<Config>,
    pub tags_provider: Arc<Provider>,
    pub body_size: usize,
    pub span_pointers: Option<Vec<SpanPointer>>,

    pub tracer_header_tags_lang: String,
    pub tracer_header_tags_lang_version: String,
    pub tracer_header_tags_lang_interpreter: String,
    pub tracer_header_tags_lang_vendor: String,
    pub tracer_header_tags_tracer_version: String,
    pub tracer_header_tags_container_id: String,
    pub tracer_header_tags_client_computed_top_level: bool,
    pub tracer_header_tags_client_computed_stats: bool,
    pub tracer_header_tags_dropped_p0_traces: usize,
    pub tracer_header_tags_dropped_p0_spans: usize,
}

/// Names of tags that can be emitted by the WAF.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TagName {
    // AppSec tags
    AppsecEnabled,
    AppsecEvent,
    AppsecEventRulesVersion,
    AppsecJson,
    AppsecWafDuration,
    AppsecWafTimeouts,
    AppsecWafVersion,
    // Hidden span tags of relevance
    Origin,
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
            Self::AppsecEventRulesVersion => "_dd.appsec.event_rules.version",
            Self::AppsecJson => "_dd.appsec.json",
            Self::AppsecWafDuration => "_dd.appsec.waf.duration",
            Self::AppsecWafTimeouts => "_dd.appsec.waf.timeouts",
            Self::AppsecWafVersion => "_dd.appsec.waf.version",
            Self::Origin => "_dd.origin",
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
    /// The special key used to encode AAP events
    AppSecEvents { triggers: Vec<serde_json::Value> },
    /// A tag value that actually is a metric value.
    Metric(f64),
}
impl std::fmt::Display for TagValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Always(str) => write!(f, "{str}"),
            Self::OnEvent(value) => write!(f, "{value}"),
            Self::AppSecEvents { triggers } => write!(
                f,
                r#"{{ "triggers": {} }}"#,
                serde_json::Value::Array(triggers.clone())
            ),
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
                    // Logging these as INFO as the user is often unable to do anything about these issues, and hence
                    // WARN is excessive.
                    Err(e) => info!("aap: unable to parse body, it will not be analyzed for security activity: {e}"),
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

fn try_parse_body(body: impl Read, content_type: &str) -> Result<WafObject, BodyParseError> {
    let mime_type: Mime = content_type.parse()?;
    try_parse_body_with_mime(body, mime_type)
}

fn try_parse_body_with_mime(body: impl Read, mime_type: Mime) -> Result<WafObject, BodyParseError> {
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
                return Err(BodyParseError::MissingBoundary(mime_type));
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
                                // Logging as INFO as this is often not directly actionnable by the
                                // customer and can lead to excessive log spam if sent as WARN.
                                info!(
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
        _ => Err(BodyParseError::UnsupportedMimeType(mime_type)),
    }
}

#[derive(Debug, thiserror::Error)]
enum BodyParseError {
    #[error("failed to parse content type: {0}")]
    ContentTypeParseError(#[from] mime::FromStrError),
    #[error("cannot parse {0} body: missing boundary parameter")]
    MissingBoundary(Mime),
    #[error("unsupported MIME type: {0}")]
    UnsupportedMimeType(Mime),
    #[error("failed to read body: {0}")]
    IOError(#[from] std::io::Error),
    #[error("failed to parse body: {0}")]
    SerdeError(Box<dyn std::error::Error>),
}
impl From<serde_json::Error> for BodyParseError {
    fn from(e: serde_json::Error) -> Self {
        Self::SerdeError(Box::new(e))
    }
}
impl From<serde_html_form::de::Error> for BodyParseError {
    fn from(e: serde_html_form::de::Error) -> Self {
        Self::SerdeError(Box::new(e))
    }
}

/// Logs a WAF run error.
///
/// This function is marked as `#[cold]` because it is almost certainly never
/// going to get called, and hence needs not be placed to optimize for a
/// near-jump.
#[cold]
fn log_waf_run_error(e: RunError) {
    warn!("aap: failed to evaluate WAF ruleset: {e}");
}

/// Depending on the `debug-assertions` setting, either calls `unreachable!` or
/// `warn!` with the specified message.
///
/// This function is marked as `#[cold]` because it is almost certainly never
/// going to get called, and hence needs not be placed to optimize for a
/// near-jump.
///
/// # Panics
/// If (and only if) `debug-assertions` are enabled in this build (not the case
/// for `release` builds, by default).
#[cold]
#[track_caller]
fn unreachable_warn(msg: &'static str) {
    if cfg!(debug_assertions) {
        unreachable!("{msg}");
    } else {
        warn!("{msg}");
    }
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
