//! Trace payloads as a tracer library POSTs them to `/v0.4/traces`.

use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use libdd_trace_protobuf::pb;
use libdd_trace_utils::tracer_header_tags::TracerHeaderTags;
use serde::Serialize;

use crate::fixtures::invocation::{
    ENV, FUNCTION_NAME, HANDLER_MS, HANDLER_START_MS, REQUEST_ID, SERVICE, TRACER_INTERPRETER,
    TRACER_LANG, TRACER_LANG_VERSION, TRACER_VERSION, VERSION, millis_to_ns,
};

pub use crate::fixtures::invocation::BASE_NS;

/// Headers the Node.js tracer sends with the reference payload ([`v04_2000_spans`], 100
/// traces). Set `X-Datadog-Trace-Count` to the trace count when sending another payload.
pub const TRACE_HEADERS: &[(&str, &str)] = &[
    ("Content-Type", "application/msgpack"),
    ("Datadog-Meta-Lang", TRACER_LANG),
    ("Datadog-Meta-Lang-Version", TRACER_LANG_VERSION),
    ("Datadog-Meta-Lang-Interpreter", TRACER_INTERPRETER),
    ("Datadog-Meta-Tracer-Version", TRACER_VERSION),
    ("X-Datadog-Trace-Count", "100"),
];

/// The current time, for consumers that drop old spans (the stats concentrator keeps a time
/// window): `make_traces_at(now_ns(), ...)`.
pub fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos()
        .try_into()
        .unwrap_or(i64::MAX)
}

/// The tracer header tags that match [`TRACE_HEADERS`].
pub fn header_tags() -> TracerHeaderTags<'static> {
    TracerHeaderTags {
        lang: TRACER_LANG,
        lang_version: TRACER_LANG_VERSION,
        lang_interpreter: TRACER_INTERPRETER,
        tracer_version: TRACER_VERSION,
        ..Default::default()
    }
}

/// `n_traces` traces of `spans_per_trace` spans each, for the invocation that starts at
/// `base_ns` (in place of [`BASE_NS`]). Every trace repeats the invocation's `aws.lambda`
/// root, which spans the handler; the other spans start 1 µs apart and last 1 ms inside it.
/// Spans 1 to 4 are SQL clients when `with_sql` is set, else HTTP clients; spans 5 to 9 are
/// HTTP clients; the rest are internal work. Trace ids start at 1_000_000.
pub fn make_traces_at(
    base_ns: i64,
    n_traces: usize,
    spans_per_trace: usize,
    with_sql: bool,
) -> Vec<Vec<pb::Span>> {
    let root_start = base_ns + millis_to_ns(HANDLER_START_MS);
    // The upper 64 bits of a 128-bit trace id start with the trace's start time in seconds.
    let tid = format!("{:08x}00000000", root_start / 1_000_000_000);
    (0..n_traces)
        .map(|t| {
            let trace_id = 1_000_000 + t as u64;
            (0..spans_per_trace)
                .map(|s| {
                    let span_id = trace_id * 1000 + s as u64 + 1;
                    let parent_id = if s == 0 { 0 } else { trace_id * 1000 + 1 };
                    let (name, resource, typ, extra): (&str, &str, &str, Vec<(&str, &str)>) =
                        match s {
                            0 => (
                                "aws.lambda",
                                FUNCTION_NAME,
                                "serverless",
                                vec![
                                    ("span.kind", "server"),
                                    ("request_id", REQUEST_ID),
                                    ("function_trigger.event_source", "api-gateway"),
                                    ("http.method", "GET"),
                                    ("http.status_code", "200"),
                                ],
                            ),
                            1..=4 if with_sql => (
                                "postgres.query",
                                "SELECT id, name FROM users WHERE id = 42 AND email = 'bob@example.com'",
                                "sql",
                                vec![
                                    ("db.type", "postgres"),
                                    ("db.name", "app"),
                                    ("out.host", "db.internal"),
                                    ("span.kind", "client"),
                                ],
                            ),
                            1..=9 => (
                                "http.request",
                                "GET /v1/items",
                                "http",
                                vec![
                                    ("http.method", "GET"),
                                    ("http.url", "https://svc.internal/v1/items/987?token=abc"),
                                    ("http.status_code", "200"),
                                    ("span.kind", "client"),
                                    ("out.host", "svc.internal"),
                                ],
                            ),
                            _ => ("internal.work", "compute", "custom", vec![("component", "app")]),
                        };
                    let mut meta: HashMap<String, String> = [
                        ("runtime-id", "f45568ad09d5480b99087d86ebda26e6"),
                        ("language", "javascript"),
                        ("_dd.p.dm", "-0"),
                        ("_dd.p.tid", tid.as_str()),
                        ("env", ENV),
                        ("version", VERSION),
                        ("component", "http"),
                    ]
                    .into_iter()
                    .chain(extra)
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                    meta.shrink_to_fit();
                    let mut metrics: HashMap<String, f64> = HashMap::new();
                    metrics.insert("process_id".to_string(), 1234.0);
                    metrics.insert("_sampling_priority_v1".to_string(), 1.0);
                    if s == 0 {
                        metrics.insert("_dd.top_level".to_string(), 1.0);
                        metrics.insert("_dd.agent_psr".to_string(), 1.0);
                    }
                    if typ == "sql" || typ == "http" {
                        metrics.insert("_dd.measured".to_string(), 1.0);
                    }
                    let (start, duration) = if s == 0 {
                        (root_start, millis_to_ns(HANDLER_MS))
                    } else {
                        (root_start + (s as i64) * 1000, 1_000_000)
                    };
                    pb::Span {
                        service: SERVICE.to_string(),
                        name: name.to_string(),
                        resource: resource.to_string(),
                        trace_id,
                        span_id,
                        parent_id,
                        start,
                        duration,
                        error: 0,
                        meta,
                        metrics,
                        r#type: typ.to_string(),
                        ..Default::default()
                    }
                })
                .collect()
        })
        .collect()
}

/// [`make_traces_at`] for the shared invocation, at [`BASE_NS`].
pub fn make_traces(n_traces: usize, spans_per_trace: usize, with_sql: bool) -> Vec<Vec<pb::Span>> {
    make_traces_at(BASE_NS, n_traces, spans_per_trace, with_sql)
}

/// The reference payload: 100 traces of 20 spans, with SQL spans.
pub fn trace_2000_spans() -> Vec<Vec<pb::Span>> {
    make_traces(100, 20, true)
}

/// A [`pb::Span`] with `meta` and `metrics` in key order, so the encoded bytes are the same on
/// every run (a `HashMap` iterates in a random order). Same fields and order as `pb::Span`
/// encodes; the generated spans have no `meta_struct`, `span_links` or `span_events`.
#[derive(Serialize)]
struct V04Span<'a> {
    service: &'a str,
    name: &'a str,
    resource: &'a str,
    trace_id: u64,
    span_id: u64,
    parent_id: u64,
    start: i64,
    duration: i64,
    #[serde(skip_serializing_if = "is_zero")]
    error: i32,
    meta: BTreeMap<&'a str, &'a str>,
    metrics: BTreeMap<&'a str, f64>,
    r#type: &'a str,
}

fn is_zero(n: &i32) -> bool {
    *n == 0
}

impl<'a> From<&'a pb::Span> for V04Span<'a> {
    fn from(s: &'a pb::Span) -> Self {
        Self {
            service: &s.service,
            name: &s.name,
            resource: &s.resource,
            trace_id: s.trace_id,
            span_id: s.span_id,
            parent_id: s.parent_id,
            start: s.start,
            duration: s.duration,
            error: s.error,
            meta: s
                .meta
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
            metrics: s.metrics.iter().map(|(k, v)| (k.as_str(), *v)).collect(),
            r#type: &s.r#type,
        }
    }
}

/// Encodes traces as a v0.4 msgpack body (`rmp_serde::to_vec_named`, as tracers do).
/// For generated spans only: `meta_struct`, `span_links` and `span_events` are not encoded.
pub fn v04_msgpack(traces: &[Vec<pb::Span>]) -> Vec<u8> {
    let traces: Vec<Vec<V04Span<'_>>> = traces
        .iter()
        .map(|t| t.iter().map(V04Span::from).collect())
        .collect();
    rmp_serde::to_vec_named(&traces).expect("encode v0.4 msgpack")
}

/// The reference v0.4 body: 2000 spans, about 1 MB.
pub fn v04_2000_spans() -> Vec<u8> {
    v04_msgpack(&trace_2000_spans())
}
