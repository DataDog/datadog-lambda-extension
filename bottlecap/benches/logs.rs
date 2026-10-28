//! Logs pipeline hot path: listener parse, per-line processing, the OOM check, stitching,
//! the aggregator batch build and the flush compression.
//!
//! Review items: 5.5 (JSON line parsed twice, copied, serialized 2 or 3 times), 5.6 (OOM check),
//! 5.7 (fixed fields serialized per line), 5.14 (zstd inline, quadratic stitch), 2.2.
//!
//! Every per-line bench runs over `LINES` events and reports with `Throughput::Elements`, so
//! criterion prints the per-line cost.

mod common;
#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

use std::io::Write as _;
use std::time::{Duration, Instant};

use bottlecap::event_bus::Event;
use bottlecap::extension::telemetry::events::{TelemetryEvent, TelemetryRecord};
use bottlecap::extension::telemetry::stitch::{FragmentBuffer, Stitch};
use bottlecap::logs::aggregator::Aggregator;
use bottlecap::logs::aggregator_service::{AggregatorHandle, AggregatorService};
use bottlecap::logs::lambda::processor::{LambdaProcessor, is_oom_error};
use bottlecap::logs::lambda::{IntakeLog, Lambda, Message};
use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use fixtures::logs;
use fixtures::telemetry::{self, LogKind, REQUEST_ID};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

/// Events per POST body, the Telemetry API `maxItems`.
const LINES: usize = 1000;

fn parse_events(body: &[u8]) -> Vec<TelemetryEvent> {
    serde_json::from_slice(body).expect("fixture body parses")
}

fn message_of(event: &TelemetryEvent) -> String {
    match &event.record {
        TelemetryRecord::Function(Value::Object(o)) => {
            serde_json::to_string(o).expect("serialize object")
        }
        TelemetryRecord::Function(Value::String(s)) => s.clone(),
        other => panic!("unexpected record {other:?}"),
    }
}

/// A processor in the state it has after `platform.initStart` and `platform.start`, with the
/// aggregator service running on `rt`.
struct Pipeline {
    processor: LambdaProcessor,
    handle: AggregatorHandle,
    tags: String,
    arn: String,
    /// `process()` awaits `event_bus.send` for platform and OOM events; drain it between
    /// iterations (see `drain_event_bus`) so a full channel never stalls the bench.
    event_bus_rx: tokio::sync::mpsc::Receiver<Event>,
}

impl Pipeline {
    fn drain_event_bus(&mut self) {
        while self.event_bus_rx.try_recv().is_ok() {}
    }
}

fn pipeline(rt: &tokio::runtime::Runtime) -> Pipeline {
    let config = common::config();
    let provider = common::provider(config.clone());
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(1024);
    let mut processor = LambdaProcessor::new(provider.clone(), config, tx, false);
    let (service, handle) = AggregatorService::default();
    rt.spawn(service.run());
    rt.block_on(async {
        for body in [
            telemetry::init_start(),
            telemetry::platform_start(REQUEST_ID),
        ] {
            let event: TelemetryEvent = serde_json::from_str(&body).expect("platform event");
            processor.process(event, &handle).await;
        }
        let _ = handle.get_batches().await;
    });
    let mut pipeline = Pipeline {
        processor,
        handle,
        tags: provider.get_tags_string(),
        arn: provider.get_canonical_id().unwrap_or_default(),
        event_bus_rx: rx,
    };
    pipeline.drain_event_bus();
    pipeline
}

/// `map_log_level_to_status` in `src/logs/lambda/processor.rs`.
fn map_log_level_to_status(level: &str) -> Option<&'static str> {
    match level.to_uppercase().as_str() {
        "FATAL" | "CRITICAL" => Some("critical"),
        "ERROR" | "ERR" => Some("error"),
        "WARN" | "WARNING" => Some("warn"),
        "INFO" | "INFORMATION" => Some("info"),
        "DEBUG" => Some("debug"),
        "TRACE" => Some("trace"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Listener parse (`Listener::handle` in src/extension/telemetry/listener.rs) and the alternatives from the review.
// ---------------------------------------------------------------------------------------------

/// Non-flatten event with a `Value` record (same tree, no `#[serde(flatten)]`).
#[derive(Deserialize)]
struct PlainEvent {
    #[allow(dead_code)]
    time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    kind: String,
    #[allow(dead_code)]
    record: Value,
}

/// Event whose record stays as raw bytes (the 5.5 prototype).
#[derive(Deserialize)]
struct RawEvent<'a> {
    #[allow(dead_code)]
    time: chrono::DateTime<chrono::Utc>,
    #[serde(rename = "type", borrow)]
    #[allow(dead_code)]
    kind: &'a str,
    #[serde(borrow)]
    record: &'a RawValue,
}

fn listener_parse(c: &mut Criterion) {
    let mut g = c.benchmark_group("logs/listener_parse");
    g.throughput(Throughput::Elements(LINES as u64));
    for kind in LogKind::ALL {
        let body = telemetry::log_batch(kind, LINES);
        g.bench_with_input(
            BenchmarkId::new("flatten_telemetry_event", kind.name()),
            &body,
            |b, body| {
                b.iter(|| parse_events(body));
            },
        );
        g.bench_with_input(
            BenchmarkId::new("plain_struct_value_record", kind.name()),
            &body,
            |b, body| {
                b.iter(|| serde_json::from_slice::<Vec<PlainEvent>>(body).expect("parse"));
            },
        );
        g.bench_with_input(
            BenchmarkId::new("raw_value_record", kind.name()),
            &body,
            |b, body| {
                b.iter(|| serde_json::from_slice::<Vec<RawEvent>>(body).expect("parse"));
            },
        );
    }
    g.finish();
}

// ---------------------------------------------------------------------------------------------
// Processor: LambdaProcessor::process per line, and the operations inside it.
// ---------------------------------------------------------------------------------------------

fn processor_process(c: &mut Criterion) {
    common::setup_env();
    let rt = common::runtime();
    let mut p = pipeline(&rt);

    let mut g = c.benchmark_group("logs/processor_process");
    g.throughput(Throughput::Elements(LINES as u64));
    for kind in LogKind::ALL {
        let events = parse_events(&telemetry::log_batch(kind, LINES));
        g.bench_function(BenchmarkId::new("per_line", kind.name()), |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let batch = events.clone();
                    rt.block_on(async {
                        let start = Instant::now();
                        for event in batch {
                            p.processor.process(event, &p.handle).await;
                        }
                        total += start.elapsed();
                        // Keep the aggregator from growing across iterations.
                        let _ = p.handle.get_batches().await;
                    });
                    p.drain_event_bus();
                }
                total
            });
        });
    }
    g.finish();
}

/// Borrowed intake log: the lower bound for one serialization with no per-line String clones.
#[derive(Serialize)]
struct LambdaRef<'a> {
    arn: &'a str,
    request_id: Option<&'a str>,
}
#[derive(Serialize)]
struct MessageRef<'a> {
    message: &'a str,
    lambda: LambdaRef<'a>,
    timestamp: i64,
    status: &'a str,
}
#[derive(Serialize)]
struct IntakeLogRef<'a> {
    message: MessageRef<'a>,
    hostname: &'a str,
    service: &'a str,
    ddtags: &'a str,
    ddsource: &'a str,
}

/// The fields `get_intake_log` reads from a JSON line, borrowed.
#[derive(Deserialize)]
struct Fields<'a> {
    #[serde(rename = "requestId", borrow, default)]
    request_id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    level: Option<std::borrow::Cow<'a, str>>,
}

fn intake_log(message: &str, arn: &str, tags: &str) -> IntakeLog {
    IntakeLog {
        message: Message {
            message: message.to_string(),
            lambda: Lambda {
                arn: arn.to_string(),
                request_id: Some(REQUEST_ID.to_string()),
                ..Lambda::default()
            },
            timestamp: 1_761_566_400_123,
            status: "info".to_string(),
        },
        hostname: arn.to_string(),
        service: "orders".to_string(),
        tags: tags.to_string(),
        source: "lambda".to_string(),
    }
}

fn processor_parts(c: &mut Criterion) {
    common::setup_env();
    let rt = common::runtime();
    let p = pipeline(&rt);
    let (arn, tags) = (p.arn.clone(), p.tags.clone());

    let mut g = c.benchmark_group("logs/processor_parts");
    g.throughput(Throughput::Elements(LINES as u64));
    for kind in LogKind::ALL {
        let body = telemetry::log_batch(kind, LINES);
        let events = parse_events(&body);
        let messages: Vec<String> = events.iter().map(message_of).collect();
        let intake: Vec<IntakeLog> = messages
            .iter()
            .map(|m| intake_log(m, &arn, &tags))
            .collect();

        // `get_message`: `let copy = event.clone()`.
        g.bench_function(BenchmarkId::new("event_clone", kind.name()), |b| {
            b.iter(|| {
                events.iter().fold(0usize, |n, e| {
                    std::hint::black_box(e.clone());
                    n + 1
                })
            });
        });
        // `get_message`: `serde_json::to_string(&obj)` for object records.
        if kind == LogKind::JsonObject {
            g.bench_function(
                BenchmarkId::new("to_string_object_record", kind.name()),
                |b| {
                    b.iter(|| {
                        events
                            .iter()
                            .map(|e| std::hint::black_box(message_of(e)).len())
                            .sum::<usize>()
                    });
                },
            );
        }
        // `get_intake_log`: `serde_json::from_str::<Value>(message)`.
        g.bench_function(
            BenchmarkId::new("reparse_message_as_value", kind.name()),
            |b| {
                b.iter(|| {
                    messages
                        .iter()
                        .filter(|m| serde_json::from_str::<Value>(m).is_ok())
                        .count()
                });
            },
        );
        // get_intake_log: `message.clone()` plus the tags, arn, service and source clones.
        g.bench_function(BenchmarkId::new("intake_field_clones", kind.name()), |b| {
            b.iter(|| {
                messages.iter().fold(0usize, |acc, m| {
                    let fields = std::hint::black_box((
                        m.clone(),
                        arn.clone(),
                        arn.clone(),
                        tags.clone(),
                        "orders".to_string(),
                        "lambda".to_string(),
                    ));
                    acc + fields.0.len()
                })
            });
        });
        // aggregator insert: `serde_json::to_string(&IntakeLog)` per line.
        g.bench_function(BenchmarkId::new("serialize_intake_log", kind.name()), |b| {
            b.iter(|| {
                intake
                    .iter()
                    .map(|l| serde_json::to_string(l).expect("serialize").len())
                    .sum::<usize>()
            });
        });
        // Lower bound: extract from the parsed tree, borrow every field, map the status, run
        // the OOM scan, serialize once. It skips the processing rules, the durable-function
        // handling and the aggregator insert, which production keeps.
        g.bench_function(
            BenchmarkId::new("alt_borrowed_single_serialize", kind.name()),
            |b| {
                b.iter_batched(
                    || events.clone(),
                    |events| {
                        let mut out = 0usize;
                        let mut oom = 0usize;
                        for e in events {
                            let (msg, rid, status) = match e.record {
                                TelemetryRecord::Function(Value::Object(o)) => {
                                    let rid = o
                                        .get("requestId")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned);
                                    let st =
                                        o.get("level").and_then(Value::as_str).map(str::to_owned);
                                    (serde_json::to_string(&o).expect("serialize"), rid, st)
                                }
                                TelemetryRecord::Function(Value::String(s)) => {
                                    if s.starts_with('{') {
                                        let f: Option<Fields> = serde_json::from_str(&s).ok();
                                        let (rid, st) = f
                                            .map(|f| {
                                                (
                                                    f.request_id.map(|c| c.into_owned()),
                                                    f.level.map(|c| c.into_owned()),
                                                )
                                            })
                                            .unwrap_or((None, None));
                                        (s, rid, st)
                                    } else {
                                        (s, None, None)
                                    }
                                }
                                _ => unreachable!(),
                            };
                            oom += usize::from(is_oom_error(&msg));
                            let log = IntakeLogRef {
                                message: MessageRef {
                                    message: &msg,
                                    lambda: LambdaRef {
                                        arn: &arn,
                                        request_id: rid.as_deref().or(Some(REQUEST_ID)),
                                    },
                                    timestamp: e.time.timestamp_millis(),
                                    status: status
                                        .as_deref()
                                        .and_then(map_log_level_to_status)
                                        .unwrap_or("info"),
                                },
                                hostname: &arn,
                                service: "orders",
                                ddtags: &tags,
                                ddsource: "lambda",
                            };
                            out += serde_json::to_string(&log).expect("serialize").len();
                        }
                        (out, oom)
                    },
                    BatchSize::LargeInput,
                );
            },
        );
        // 5.5 prototype, same lower bound: RawValue parse + borrowed field extraction + status
        // map + OOM scan + one serialization.
        g.bench_function(
            BenchmarkId::new("alt_raw_value_end_to_end", kind.name()),
            |b| {
                b.iter(|| {
                    let mut out = 0usize;
                    let mut oom = 0usize;
                    let parsed: Vec<RawEvent> = serde_json::from_slice(&body).expect("parse");
                    for e in parsed {
                        let raw = e.record.get();
                        let (msg, rid, status): (
                            std::borrow::Cow<str>,
                            Option<String>,
                            Option<String>,
                        ) = if raw.starts_with('{') {
                            let f: Fields = serde_json::from_str(raw).expect("fields");
                            (
                                std::borrow::Cow::Borrowed(raw),
                                f.request_id.map(|c| c.into_owned()),
                                f.level.map(|c| c.into_owned()),
                            )
                        } else {
                            let s: String = serde_json::from_str(raw).expect("string record");
                            if s.starts_with('{') {
                                let f: Option<Fields> = serde_json::from_str(&s).ok();
                                let (rid, st) = f
                                    .map(|f| {
                                        (
                                            f.request_id.map(|c| c.into_owned()),
                                            f.level.map(|c| c.into_owned()),
                                        )
                                    })
                                    .unwrap_or((None, None));
                                (std::borrow::Cow::Owned(s), rid, st)
                            } else {
                                (std::borrow::Cow::Owned(s), None, None)
                            }
                        };
                        oom += usize::from(is_oom_error(&msg));
                        let log = IntakeLogRef {
                            message: MessageRef {
                                message: &msg,
                                lambda: LambdaRef {
                                    arn: &arn,
                                    request_id: rid.as_deref().or(Some(REQUEST_ID)),
                                },
                                timestamp: e.time.timestamp_millis(),
                                status: status
                                    .as_deref()
                                    .and_then(map_log_level_to_status)
                                    .unwrap_or("info"),
                            },
                            hostname: &arn,
                            service: "orders",
                            ddtags: &tags,
                            ddsource: "lambda",
                        };
                        out += serde_json::to_string(&log).expect("serialize").len();
                    }
                    (out, oom)
                });
            },
        );
        // 5.7: serialize only the per-line fields, append the fixed suffix once computed.
        let suffix = {
            let mut s = String::from(",\"hostname\":");
            s.push_str(&serde_json::to_string(&arn).expect("arn"));
            s.push_str(",\"service\":\"orders\",\"ddtags\":");
            s.push_str(&serde_json::to_string(&tags).expect("tags"));
            s.push_str(",\"ddsource\":\"lambda\"}");
            s
        };
        let arn_json = serde_json::to_string(&arn).expect("arn");
        let prebuilt = |m: &str| -> Vec<u8> {
            let mut out = Vec::with_capacity(m.len() + suffix.len() + 160);
            out.extend_from_slice(b"{\"message\":{\"message\":");
            serde_json::to_writer(&mut out, m).expect("message");
            out.extend_from_slice(b",\"lambda\":{\"arn\":");
            out.extend_from_slice(arn_json.as_bytes());
            out.extend_from_slice(b",\"request_id\":\"");
            out.extend_from_slice(REQUEST_ID.as_bytes());
            out.extend_from_slice(b"\"},\"timestamp\":1761566400123,\"status\":\"info\"}");
            out.extend_from_slice(suffix.as_bytes());
            out
        };
        assert_eq!(
            String::from_utf8(prebuilt(&messages[0])).expect("utf-8"),
            serde_json::to_string(&intake[0]).expect("serialize"),
            "the prebuilt suffix must give the same bytes as serde"
        );
        g.bench_function(
            BenchmarkId::new("alt_prebuilt_suffix_serialize", kind.name()),
            |b| {
                b.iter(|| messages.iter().map(|m| prebuilt(m).len()).sum::<usize>());
            },
        );
    }

    // `process()`: one `Vec` and one unbounded channel send per line.
    g.bench_function("per_line_vec_and_unbounded_send", |b| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<String>>();
        let lines: Vec<String> = (0..LINES).map(|i| i.to_string()).collect();
        b.iter_batched(
            || lines.clone(),
            |lines| {
                for s in lines {
                    tx.send(vec![s]).expect("send");
                }
                while rx.try_recv().is_ok() {}
            },
            BatchSize::LargeInput,
        );
    });
    g.finish();
}

// ---------------------------------------------------------------------------------------------
// OOM check: 7 substring scans per line (`is_oom_error`) vs one regex alternation (5.6).
// ---------------------------------------------------------------------------------------------

/// `OOM_ERRORS` in `src/logs/lambda/processor.rs`, the needles `is_oom_error` searches for.
const OOM_NEEDLES: [&str; 7] = [
    "fatal error: runtime: out of memory",
    "java.lang.OutOfMemoryError",
    "JavaScript heap out of memory",
    "Runtime exited with error: signal: killed",
    "MemoryError",
    "failed to allocate memory (NoMemoryError)",
    "OutOfMemoryException",
];

fn oom_check(c: &mut Criterion) {
    let mut g = c.benchmark_group("logs/oom_check");
    g.throughput(Throughput::Elements(LINES as u64));
    let regex = regex::Regex::new(
        &OOM_NEEDLES
            .iter()
            .map(|n| regex::escape(n))
            .collect::<Vec<_>>()
            .join("|"),
    )
    .expect("oom regex");
    // The alternative must agree with `is_oom_error` on every hit and on 1000 clean lines.
    for line in logs::OOM_LINES {
        assert!(is_oom_error(line) && regex.is_match(line), "{line}");
    }
    for i in 0..LINES {
        let line = logs::text_line(i);
        assert!(!is_oom_error(&line) && !regex.is_match(&line), "{line}");
    }
    for len in [200usize, 2000] {
        let lines: Vec<String> = (0..LINES).map(|i| logs::text_line_of_len(i, len)).collect();
        g.bench_with_input(
            BenchmarkId::new("is_oom_error_7_contains", len),
            &lines,
            |b, lines| {
                b.iter(|| lines.iter().filter(|l| is_oom_error(l)).count());
            },
        );
        g.bench_with_input(
            BenchmarkId::new("alt_single_regex", len),
            &lines,
            |b, lines| {
                b.iter(|| lines.iter().filter(|l| regex.is_match(l)).count());
            },
        );
    }
    let hits: Vec<String> = (0..LINES)
        .map(|i| logs::OOM_LINES[i % logs::OOM_LINES.len()].to_string())
        .collect();
    g.bench_function("is_oom_error_7_contains/hit", |b| {
        b.iter(|| hits.iter().filter(|l| is_oom_error(l)).count());
    });
    g.finish();
}

// ---------------------------------------------------------------------------------------------
// Stitching split records (src/extension/telemetry/stitch.rs). Review 5.14, 2.2.
// ---------------------------------------------------------------------------------------------

fn stitch(c: &mut Criterion) {
    let mut g = c.benchmark_group("logs/stitch");
    g.sample_size(20);
    let piece = 256 << 10;
    for (label, total) in [("1mib", 1usize << 20), ("4mib", 4usize << 20)] {
        let single = telemetry::split_record_single_post(total, piece);
        g.throughput(Throughput::Bytes(single.len() as u64));
        // The listener tries a full parse first (`Listener::handle`); it fails on a split body.
        g.bench_with_input(
            BenchmarkId::new("failed_full_parse", label),
            &single,
            |b, body| {
                b.iter(|| serde_json::from_slice::<Vec<TelemetryEvent>>(body).is_err());
            },
        );
        g.bench_with_input(
            BenchmarkId::new("single_post_seams", label),
            &single,
            |b, body| {
                b.iter(|| match FragmentBuffer::default().stitch(body) {
                    Stitch::Complete(events) => events.len(),
                    other => panic!("expected complete, got {other:?}"),
                });
            },
        );
        let posts = telemetry::split_record_posts(total, piece, 6);
        g.bench_with_input(
            BenchmarkId::new("across_posts", label),
            &posts,
            |b, posts| {
                b.iter(|| {
                    let fragments = FragmentBuffer::default();
                    let mut events = 0;
                    for post in posts {
                        if let Stitch::Complete(e) = fragments.stitch(post) {
                            events += e.len();
                        }
                    }
                    events
                });
            },
        );
    }
    g.finish();
}

// ---------------------------------------------------------------------------------------------
// Aggregator batch build (src/logs/aggregator.rs) and flush compression (`LogsFlusher::encode`).
// ---------------------------------------------------------------------------------------------

fn intake_lines(kind: LogKind, n: usize, arn: &str, tags: &str) -> Vec<String> {
    (0..n)
        .map(|i| {
            let message = match kind {
                LogKind::Text => logs::text_line(i),
                _ => logs::json_line(i),
            };
            serde_json::to_string(&intake_log(&message, arn, tags)).expect("serialize")
        })
        .collect()
}

fn drain(agg: &mut Aggregator) -> Vec<Vec<u8>> {
    let mut batches = Vec::new();
    loop {
        let batch = agg.get_batch();
        if batch.is_empty() {
            break;
        }
        batches.push(batch);
    }
    batches
}

fn aggregator(c: &mut Criterion) {
    common::setup_env();
    let config = common::config();
    let provider = common::provider(config);
    let (arn, tags) = (
        provider.get_canonical_id().unwrap_or_default(),
        provider.get_tags_string(),
    );
    const N: usize = 10_000;

    let mut g = c.benchmark_group("logs/aggregator");
    g.throughput(Throughput::Elements(N as u64));
    for kind in [LogKind::Text, LogKind::JsonObject] {
        let lines = intake_lines(kind, N, &arn, &tags);
        g.bench_function(
            BenchmarkId::new("add_batch_and_get_batches", kind.name()),
            |b| {
                b.iter_batched(
                    || lines.clone(),
                    |lines| {
                        let mut agg = Aggregator::default();
                        agg.add_batch(lines);
                        drain(&mut agg).len()
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    g.finish();
}

fn stream_encode(data: &[u8], level: i32) -> usize {
    // `LogsFlusher::encode` in src/logs/flusher.rs
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), level).expect("encoder");
    encoder.write_all(data).expect("write");
    encoder.finish().expect("finish").len()
}

fn compress(c: &mut Criterion) {
    common::setup_env();
    let config = common::config();
    let provider = common::provider(config.clone());
    let (arn, tags) = (
        provider.get_canonical_id().unwrap_or_default(),
        provider.get_tags_string(),
    );
    let level = config.logs_config_compression_level;

    let mut g = c.benchmark_group("logs/compress");
    g.sample_size(20);
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    for kind in [LogKind::Text, LogKind::JsonObject] {
        let mut agg = Aggregator::default();
        agg.add_batch(intake_lines(kind, 5000, &arn, &tags));
        let batch = drain(&mut agg).remove(0);
        inputs.push((format!("{}_batch", kind.name()), batch));
    }
    // Real logs compress far less than synthetic ones: a batch of pseudo-random hex words.
    let noisy: Vec<String> = (0..1000)
        .map(|i| {
            let mut x: u64 = 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(i as u64 + 1);
            let mut msg = String::with_capacity(1600);
            while msg.len() < 1500 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                msg.push_str(&format!("{x:x} "));
            }
            serde_json::to_string(&intake_log(&msg, &arn, &tags)).expect("serialize")
        })
        .collect();
    let mut agg = Aggregator::default();
    agg.add_batch(noisy);
    inputs.push(("noisy_batch".to_string(), drain(&mut agg).remove(0)));

    for (label, data) in &inputs {
        g.throughput(Throughput::Bytes(data.len() as u64));
        println!(
            "logs/compress {label}: {} B raw, zstd level {level} (config default) -> {} B",
            data.len(),
            stream_encode(data, level)
        );
        g.bench_with_input(
            BenchmarkId::new("zstd_stream_config_level", label),
            data,
            |b, data| {
                b.iter(|| stream_encode(data, level));
            },
        );
        g.bench_with_input(
            BenchmarkId::new("zstd_stream_l3", label),
            data,
            |b, data| {
                b.iter(|| stream_encode(data, 3));
            },
        );
        g.bench_with_input(
            BenchmarkId::new("zstd_stream_l1", label),
            data,
            |b, data| {
                b.iter(|| stream_encode(data, 1));
            },
        );
        g.bench_with_input(
            BenchmarkId::new("alt_zstd_bulk_l3_reused_ctx", label),
            data,
            |b, data| {
                let mut compressor = zstd::bulk::Compressor::new(3).expect("compressor");
                b.iter(|| compressor.compress(data).expect("compress").len());
            },
        );
    }
    g.finish();
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = listener_parse, processor_process, processor_parts, oom_check, stitch, aggregator, compress
}
criterion_main!(benches);
