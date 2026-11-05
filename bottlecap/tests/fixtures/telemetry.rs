//! Telemetry API POST bodies: platform events and function log lines, whole and split.
//!
//! Bottlecap subscribes with `maxItems: 1000`, `maxBytes: 1 MiB`, `timeoutMs: 25`. Lambda
//! cuts a log record longer than about 256 KiB into pieces, each an event of its own, and a
//! POST can end on a cut piece. `split_record_posts` reproduces that shape.

use serde_json::Value;

use crate::fixtures::invocation::{
    FUNCTION_VERSION, HANDLER_MS, HANDLER_START_MS, INIT_TYPE, MEMORY_SIZE_MB, REGION,
    RUNTIME_DONE_MS, RUNTIME_VERSION, START_MS, log_millis, time,
};
use crate::fixtures::logs;

// Callers also reach these through `telemetry`; not every crate that includes it uses them.
#[allow(unused_imports)]
pub use crate::fixtures::invocation::{FUNCTION_ARN, REQUEST_ID, XRAY_TRACE_HEADER};

/// Which shape a function log record takes in the Telemetry API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogKind {
    /// `"record": "<text line>"` (text log format, about 200 B).
    Text,
    /// `"record": { ... }` (JSON log format, the record is already an object, about 2 KB).
    JsonObject,
    /// `"record": "{ ... }"` (text log format carrying a JSON document, about 2 KB).
    JsonString,
}

impl LogKind {
    pub const ALL: [LogKind; 3] = [LogKind::Text, LogKind::JsonObject, LogKind::JsonString];

    pub fn name(self) -> &'static str {
        match self {
            LogKind::Text => "text",
            LogKind::JsonObject => "json_object",
            LogKind::JsonString => "json_string",
        }
    }
}

/// One telemetry event `millis` after `BASE_NS`:
/// `{"time":"...","type":<kind>,"record":<record_json>}`.
pub fn event(millis: usize, kind: &str, record_json: &str) -> String {
    format!(
        r#"{{"time":"{}","type":"{kind}","record":{record_json}}}"#,
        time(millis)
    )
}

pub fn init_start() -> String {
    event(
        0,
        "platform.initStart",
        &format!(
            r#"{{"initializationType":"{INIT_TYPE}","phase":"init","runtimeVersion":"{RUNTIME_VERSION}","runtimeVersionArn":"arn:aws:lambda:{REGION}::runtime:abc"}}"#
        ),
    )
}

pub fn init_runtime_done() -> String {
    event(
        40,
        "platform.initRuntimeDone",
        &format!(r#"{{"initializationType":"{INIT_TYPE}","phase":"init","status":"success"}}"#),
    )
}

pub fn init_report() -> String {
    event(
        41,
        "platform.initReport",
        &format!(
            r#"{{"initializationType":"{INIT_TYPE}","phase":"init","status":"success","metrics":{{"durationMs":38.12}}}}"#
        ),
    )
}

pub fn platform_start(request_id: &str) -> String {
    event(
        START_MS,
        "platform.start",
        &format!(
            r#"{{"requestId":"{request_id}","version":"{FUNCTION_VERSION}","tracing":{{"spanId":"54565fb41ac79632","type":"X-Amzn-Trace-Id","value":"{XRAY_TRACE_HEADER}"}}}}"#
        ),
    )
}

/// The `responseLatency` span of `platform.runtimeDone`: the handler.
fn response_latency_span() -> String {
    format!(
        r#"{{"name":"responseLatency","start":"{}","durationMs":{HANDLER_MS}.0}}"#,
        time(HANDLER_START_MS)
    )
}

pub fn runtime_done(request_id: &str) -> String {
    event(
        RUNTIME_DONE_MS,
        "platform.runtimeDone",
        &format!(
            r#"{{"requestId":"{request_id}","status":"success","spans":[{},{{"name":"responseDuration","start":"{}","durationMs":1.0}}],"metrics":{{"durationMs":98.5,"producedBytes":1024}}}}"#,
            response_latency_span(),
            time(HANDLER_START_MS + HANDLER_MS)
        ),
    )
}

/// `platform.runtimeDone` for `request_id` with `status`; `with_metrics = false`
/// omits the `metrics` object, as Lambda does on some failed invocations.
pub fn runtime_done_with(request_id: &str, status: &str, with_metrics: bool) -> String {
    let metrics = if with_metrics {
        r#","metrics":{"durationMs":98.5,"producedBytes":1024}"#
    } else {
        ""
    };
    let error = if status == "success" {
        String::new()
    } else {
        r#","errorType":"Runtime.ExitError""#.to_string()
    };
    event(
        RUNTIME_DONE_MS,
        "platform.runtimeDone",
        &format!(
            r#"{{"requestId":"{request_id}","status":"{status}","spans":[{}]{metrics}{error}}}"#,
            response_latency_span()
        ),
    )
}

/// `platform.logsDropped` for `n` records, as Lambda sends when its buffer overflows.
pub fn logs_dropped(n: u64) -> String {
    event(
        RUNTIME_DONE_MS - 1,
        "platform.logsDropped",
        &format!(
            r#"{{"droppedBytes":{},"droppedRecords":{n},"reason":"Consumer seems to have fallen behind"}}"#,
            n * 1024
        ),
    )
}

/// An event of a type the extension does not know. Must never break a batch.
pub fn unknown_event() -> String {
    event(
        START_MS + 20,
        "platform.somethingNew",
        r#"{"field":"value"}"#,
    )
}

/// `platform.report` of a cold start such as [`REQUEST_ID`]: it has `initDurationMs`.
pub fn report(request_id: &str) -> String {
    report_with(request_id, r#","initDurationMs":38.12"#)
}

/// `platform.report` of a warm invocation: Lambda omits `initDurationMs`. [`REQUEST_ID`] is a
/// cold start, so use another request id.
pub fn warm_report(request_id: &str) -> String {
    report_with(request_id, "")
}

fn report_with(request_id: &str, init_duration: &str) -> String {
    event(
        RUNTIME_DONE_MS + 1,
        "platform.report",
        &format!(
            r#"{{"requestId":"{request_id}","status":"success","metrics":{{"durationMs":98.5,"billedDurationMs":99,"memorySizeMB":{MEMORY_SIZE_MB},"maxMemoryUsedMB":64{init_duration}}}}}"#
        ),
    )
}

/// One function log event of the given shape.
pub fn function_event(kind: LogKind, i: usize) -> String {
    let record = match kind {
        LogKind::Text => serde_json::to_string(&Value::String(logs::text_line(i))),
        LogKind::JsonObject => serde_json::to_string(&logs::json_object(i)),
        LogKind::JsonString => serde_json::to_string(&Value::String(logs::json_line(i))),
    }
    .expect("serialize function record");
    event(log_millis(i), "function", &record)
}

/// An `extension` log event (bottlecap's own stdout as Lambda re-ingests it), in the JSON
/// shape `logger::Formatter` writes.
pub fn extension_event(i: usize) -> String {
    let line =
        format!(r#"{{"status":"DEBUG","message":"DD_EXTENSION | DEBUG | processed batch {i}"}}"#);
    event(
        log_millis(i),
        "extension",
        &serde_json::to_string(&Value::String(line)).expect("serialize extension record"),
    )
}

/// Joins events into one POST body.
pub fn batch(events: &[String]) -> Vec<u8> {
    let mut body =
        Vec::with_capacity(events.iter().map(String::len).sum::<usize>() + events.len() + 2);
    body.push(b'[');
    for (i, e) in events.iter().enumerate() {
        if i > 0 {
            body.push(b',');
        }
        body.extend_from_slice(e.as_bytes());
    }
    body.push(b']');
    body
}

/// The platform events of one cold-start invocation, with no function logs.
pub fn platform_batch(request_id: &str) -> Vec<u8> {
    batch(&[
        init_start(),
        init_runtime_done(),
        init_report(),
        platform_start(request_id),
        runtime_done(request_id),
        report(request_id),
    ])
}

/// `n` function log events of one shape, as one POST body. Lambda caps a POST at 1000 events
/// and 1 MiB, so keep `n` at most 600 for the JSON shapes.
pub fn log_batch(kind: LogKind, n: usize) -> Vec<u8> {
    let events: Vec<String> = (0..n).map(|i| function_event(kind, i)).collect();
    batch(&events)
}

/// `n` function log events split into POST bodies of `per_post` events each.
pub fn log_batches(kind: LogKind, n: usize, per_post: usize) -> Vec<Vec<u8>> {
    let events: Vec<String> = (0..n).map(|i| function_event(kind, i)).collect();
    events.chunks(per_post.max(1)).map(batch).collect()
}

/// The [`REQUEST_ID`] invocation in the POST that follows its init events: `platform.start`,
/// `n` function lines, `platform.runtimeDone`, and the cold-start `platform.report`.
pub fn invocation_batch(kind: LogKind, n: usize) -> Vec<u8> {
    let mut events = Vec::with_capacity(n + 3);
    events.push(platform_start(REQUEST_ID));
    events.extend((0..n).map(|i| function_event(kind, i)));
    events.push(runtime_done(REQUEST_ID));
    events.push(report(REQUEST_ID));
    batch(&events)
}

/// A JSON record of short key/value pairs, about `total_bytes` long.
pub fn large_json_record(total_bytes: usize) -> String {
    let mut rec = String::with_capacity(total_bytes + 64);
    rec.push('{');
    let mut i = 0;
    while rec.len() < total_bytes.saturating_sub(64) {
        if i > 0 {
            rec.push(',');
        }
        rec.push_str(&format!("\"k{i}\":\"value {i}\""));
        i += 1;
    }
    rec.push('}');
    rec
}

/// Cuts `record` into `function` events of `piece_bytes` each, the way Lambda does: every
/// piece has its own envelope, timed as the `n`th log line, and carries a raw slice of the
/// record's bytes.
pub fn record_pieces(record: &str, piece_bytes: usize) -> Vec<String> {
    record
        .as_bytes()
        .chunks(piece_bytes.max(1))
        .enumerate()
        .map(|(n, chunk)| {
            event(
                log_millis(n),
                "function",
                std::str::from_utf8(chunk).expect("fixture record is ASCII"),
            )
        })
        .collect()
}

/// All pieces of one split record plus a `platform.runtimeDone` in a single POST body. The
/// body does not parse as a whole; the stitcher must drop the seams between the pieces.
pub fn split_record_single_post(total_bytes: usize, piece_bytes: usize) -> Vec<u8> {
    let mut events = record_pieces(&large_json_record(total_bytes), piece_bytes);
    events.push(runtime_done(REQUEST_ID));
    batch(&events)
}

/// A split record spread over several POST bodies of `pieces_per_post` pieces each. Every
/// body but the last ends on a cut piece; the last one carries the rest of the record and a
/// `platform.runtimeDone`. Feed them to `FragmentBuffer::stitch` in order.
pub fn split_record_posts(
    total_bytes: usize,
    piece_bytes: usize,
    pieces_per_post: usize,
) -> Vec<Vec<u8>> {
    let pieces = record_pieces(&large_json_record(total_bytes), piece_bytes);
    let mut posts: Vec<Vec<String>> = pieces
        .chunks(pieces_per_post.max(1))
        .map(<[String]>::to_vec)
        .collect();
    if let Some(last) = posts.last_mut() {
        last.push(runtime_done(REQUEST_ID));
    }
    posts.iter().map(|p| batch(p)).collect()
}
