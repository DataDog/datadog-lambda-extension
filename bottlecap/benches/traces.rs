//! Traces hot path on a 2000-span v0.4 POST: decode, dedup, `process_traces`, tag injection,
//! the stats copy, and the flush encode and compress.
//!
//! Review items: 5.1, 5.2, 5.3, 5.4, 5.11, 5.16, 5.17.

mod common;
#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bottlecap::config::Config;
use bottlecap::traces::span_dedup::{DedupKey, Deduper};
use bottlecap::traces::span_dedup_service::DedupService;
use bottlecap::traces::stats_concentrator_service::StatsConcentratorService;
use bottlecap::traces::stats_generator::StatsGenerator;
use bottlecap::traces::trace_aggregator::SendDataBuilderInfo;
use bottlecap::traces::trace_processor::{self, ServerlessTraceProcessor, TraceProcessor};
use criterion::measurement::WallTime;
use criterion::{
    BatchSize, BenchmarkGroup, Criterion, Throughput, criterion_group, criterion_main,
};
use libdd_capabilities::MaybeSend;
use libdd_capabilities::http::{HttpClientCapability, HttpError};
use libdd_capabilities::sleep::SleepCapability;
use libdd_common::http_common;
use libdd_trace_obfuscation::obfuscate::obfuscate_span;
use libdd_trace_obfuscation::obfuscation_config::{HttpConfig, ObfuscationConfig};
use libdd_trace_protobuf::pb;
use libdd_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
use libdd_trace_utils::trace_utils;
use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
use prost::Message;

const SPANS: u64 = 2000;
const N_TRACES: usize = 100;

fn group(c: &mut Criterion) -> BenchmarkGroup<'_, WallTime> {
    let mut g = c.benchmark_group("traces");
    g.throughput(Throughput::Elements(SPANS));
    g
}

fn make_config(edit: impl FnOnce(&mut Config)) -> Arc<Config> {
    let mut config = (*common::config()).clone();
    edit(&mut config);
    Arc::new(config)
}

/// Built as `build_trace_agent` in `src/startup.rs` does, error sampler off.
fn processor(config: &Config) -> ServerlessTraceProcessor {
    ServerlessTraceProcessor {
        obfuscation_config: Arc::new(ObfuscationConfig {
            tag_replace_rules: config.apm_replace_tags.clone(),
            http: HttpConfig {
                remove_paths_with_digits: config
                    .apm_config_obfuscation_http_remove_paths_with_digits,
                remove_query_string: config.apm_config_obfuscation_http_remove_query_string,
            },
            ..Default::default()
        }),
        error_sampler: trace_processor::new_error_sampler(false),
        error_rare_sample_tracer_drop: false,
    }
}

/// One untimed `process_traces` call, for setup.
fn process(
    config: &Arc<Config>,
    traces: Vec<Vec<pb::Span>>,
    body_len: usize,
) -> (Option<SendDataBuilderInfo>, TracerPayloadCollection) {
    processor(config).process_traces(
        config.clone(),
        common::provider(config.clone()),
        fixtures::traces::header_tags(),
        traces,
        body_len,
        None,
    )
}

fn decode(c: &mut Criterion) {
    let rt = common::runtime();
    let body = bytes::Bytes::from(fixtures::traces::v04_2000_spans());
    let mut g = group(c);

    // `TraceAgent::handle_traces` in src/traces/trace_agent.rs
    g.bench_function("decode_v04/libdd_from_request_body", |b| {
        b.to_async(&rt).iter_with_large_drop(|| async {
            trace_utils::get_traces_from_request_body(http_common::Body::from_bytes(body.clone()))
                .await
                .expect("decode v0.4")
        });
    });
    g.bench_function("decode_v04/libdd_from_slice", |b| {
        b.iter_with_large_drop(|| {
            libdd_trace_utils::msgpack_decoder::v04::from_slice(&body).expect("decode v0.4")
        });
    });
    g.bench_function("decode_v04/libdd_from_slice_to_owned", |b| {
        b.iter_with_large_drop(|| {
            let (traces, _) =
                libdd_trace_utils::msgpack_decoder::v04::from_slice(&body).expect("decode v0.4");
            traces
                .into_iter()
                .map(|trace| {
                    trace
                        .into_iter()
                        .map(|s| pb::Span {
                            service: s.service.to_string(),
                            name: s.name.to_string(),
                            resource: s.resource.to_string(),
                            #[allow(clippy::cast_possible_truncation)]
                            trace_id: s.trace_id as u64,
                            span_id: s.span_id,
                            parent_id: s.parent_id,
                            start: s.start,
                            duration: s.duration,
                            error: s.error,
                            meta: s
                                .meta
                                .iter()
                                .map(|(k, v)| (k.to_string(), v.to_string()))
                                .collect(),
                            metrics: s.metrics.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
                            r#type: s.r#type.to_string(),
                            ..Default::default()
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
    });
    g.bench_function("decode_v04/rmp_serde_from_slice", |b| {
        b.iter_with_large_drop(|| {
            rmp_serde::from_slice::<Vec<Vec<pb::Span>>>(&body).expect("decode v0.4")
        });
    });
    g.finish();
}

fn dedup(c: &mut Criterion) {
    let rt = common::runtime();
    let traces = fixtures::traces::trace_2000_spans();
    let keys: Arc<Vec<(u64, u64)>> = Arc::new(
        traces
            .iter()
            .flatten()
            .map(|s| (s.trace_id, s.span_id))
            .collect(),
    );

    // Review 5.16: the window holds 100 keys, so a resend of the same payload slips through.
    let mut deduper = Deduper::new(100);
    for &(trace_id, span_id) in keys.iter() {
        deduper.check_and_add(DedupKey::new(trace_id, span_id));
    }
    let caught = keys
        .iter()
        .filter(|&&(trace_id, span_id)| !deduper.check_and_add(DedupKey::new(trace_id, span_id)))
        .count();
    println!(
        "dedup: resend of the same {SPANS}-span payload, {caught} duplicates caught (capacity 100)"
    );

    let mut g = group(c);
    // One round trip per span, as `TraceAgent::handle_traces` does. Keys change every round so
    // every check is a miss.
    for (name, timeout) in [
        ("no_timeout", None),
        ("timeout_5s", Some(Duration::from_secs(5))),
    ] {
        let (service, handle) = DedupService::new();
        rt.spawn(service.run());
        let mut round = 0u64;
        g.bench_function(format!("dedup/service_round_trip/{name}"), |b| {
            b.iter_custom(|iters| {
                let first = round + 1;
                round += iters;
                let (handle, keys) = (handle.clone(), keys.clone());
                rt.block_on(rt.spawn(async move {
                    let start = Instant::now();
                    for r in first..first + iters {
                        for &(trace_id, span_id) in keys.iter() {
                            let key = DedupKey::new(trace_id + r * 1_000_000_000, span_id);
                            black_box(handle.check_and_add(key, timeout).await.ok());
                        }
                    }
                    start.elapsed()
                }))
                .expect("dedup task")
            });
        });
    }

    let inline = std::sync::Mutex::new(Deduper::new(100));
    let mut round = 0u64;
    g.bench_function("dedup/inline_mutex", |b| {
        b.iter(|| {
            round += 1;
            let mut deduper = inline.lock().expect("dedup lock");
            for &(trace_id, span_id) in keys.iter() {
                let key = DedupKey::new(trace_id + round * 1_000_000_000, span_id);
                black_box(deduper.check_and_add(key));
            }
        });
    });
    g.finish();
}

fn process_traces(c: &mut Criterion) {
    common::setup_env();
    let traces = fixtures::traces::trace_2000_spans();
    let body_len = fixtures::traces::v04_msgpack(&traces).len();
    let variants = [
        ("default", make_config(|_| {})),
        // Review 5.3: each regex compiles once per root span (`span_matches_tag_regex` in trace_processor.rs).
        (
            "regex_filters_2",
            make_config(|c| {
                c.apm_filter_tags_regex_require = Some(vec!["service:^my-.*$".to_string()]);
                c.apm_filter_tags_regex_reject = Some(vec!["http.url:.*healthcheck.*".to_string()]);
            }),
        ),
        (
            "compute_stats",
            make_config(|c| c.ext.lambda_extension_compute_stats = true),
        ),
    ];

    let mut g = group(c);
    for (name, config) in variants {
        let provider = common::provider(config.clone());
        let processor = processor(&config);
        let (info, _) = process(&config, traces.clone(), body_len);
        assert!(info.is_some(), "{name}: every chunk was dropped");
        g.bench_function(format!("process_traces/{name}"), |b| {
            b.iter_batched(
                || traces.clone(),
                |input| {
                    processor.process_traces(
                        config.clone(),
                        provider.clone(),
                        fixtures::traces::header_tags(),
                        input,
                        body_len,
                        None,
                    )
                },
                BatchSize::LargeInput,
            );
        });
    }
    g.finish();
}

fn parts(c: &mut Criterion) {
    common::setup_env();
    let config = make_config(|_| {});
    let provider = common::provider(config.clone());
    let traces = fixtures::traces::trace_2000_spans();
    let body_len = fixtures::traces::v04_msgpack(&traces).len();
    let (_, stats) = process(&config, traces.clone(), body_len);
    let TracerPayloadCollection::V07(stats_vec) = stats else {
        unreachable!("process_traces returns V07")
    };
    let tags = provider.get_tags_map().clone();
    println!("traces: {} tags injected into every span", tags.len());

    let mut g = group(c);
    // Review 5.2: the copy for stats generation in `process_traces`, dropped unused
    // when compute_stats is off.
    g.bench_function("stats_copy/clone", |b| {
        b.iter_with_large_drop(|| stats_vec.clone())
    });
    g.bench_function("stats_copy/drop", |b| {
        b.iter_batched(|| stats_vec.clone(), drop, BatchSize::LargeInput);
    });
    // `process_traces` body size: prost `encoded_len` over every payload.
    g.bench_function("body_size_encoded_len", |b| {
        b.iter(|| stats_vec.iter().map(Message::encoded_len).sum::<usize>());
    });
    // `trace.to_vec()` in libdd `collect_pb_trace_chunks` .
    g.bench_function("input_clone", |b| b.iter_with_large_drop(|| traces.clone()));

    // `ChunkProcessor::process` in src/traces/trace_processor.rs
    g.bench_function("tag_injection", |b| {
        b.iter_batched(
            || traces.clone(),
            |mut input| {
                for span in input.iter_mut().flatten() {
                    tags.iter().for_each(|(k, v)| {
                        span.meta.insert(k.clone(), v.clone());
                    });
                    span.meta
                        .insert("_dd.compute_stats".to_string(), "1".to_string());
                    span.meta.insert("origin".to_string(), "lambda".to_string());
                    span.meta
                        .insert("_dd.origin".to_string(), "lambda".to_string());
                }
                input
            },
            BatchSize::LargeInput,
        );
    });
    g.bench_function("tag_injection/reserve_get_mut", |b| {
        b.iter_batched(
            || traces.clone(),
            |mut input| {
                for span in input.iter_mut().flatten() {
                    span.meta.reserve(tags.len() + 3);
                    for (k, v) in &tags {
                        match span.meta.get_mut(k) {
                            Some(existing) => existing.clone_from(v),
                            None => {
                                span.meta.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    span.meta
                        .insert("_dd.compute_stats".to_string(), "1".to_string());
                    span.meta.insert("origin".to_string(), "lambda".to_string());
                    span.meta
                        .insert("_dd.origin".to_string(), "lambda".to_string());
                }
                input
            },
            BatchSize::LargeInput,
        );
    });
    // Upper bound only: tags on the root span alone is a behavior change.
    g.bench_function("tag_injection/root_span_only", |b| {
        b.iter_batched(
            || traces.clone(),
            |mut input| {
                for chunk in &mut input {
                    if let Some(root) = chunk.first_mut() {
                        tags.iter().for_each(|(k, v)| {
                            root.meta.insert(k.clone(), v.clone());
                        });
                    }
                    for span in chunk.iter_mut() {
                        span.meta
                            .insert("_dd.compute_stats".to_string(), "1".to_string());
                        span.meta.insert("origin".to_string(), "lambda".to_string());
                        span.meta
                            .insert("_dd.origin".to_string(), "lambda".to_string());
                    }
                }
                input
            },
            BatchSize::LargeInput,
        );
    });

    // The `obfuscate_span` call in `ChunkProcessor::process`.
    let obfuscation = processor(&config).obfuscation_config;
    g.bench_function("obfuscate_span", |b| {
        b.iter_batched(
            || traces.clone(),
            |mut input| {
                for span in input.iter_mut().flatten() {
                    obfuscate_span(span, &obfuscation);
                }
                input
            },
            BatchSize::LargeInput,
        );
    });
    // libdd `collect_pb_trace_chunks` .
    g.bench_function("normalize_and_top_level", |b| {
        b.iter_batched(
            || traces.clone(),
            |mut input| {
                for trace in &mut input {
                    black_box(libdd_trace_normalization::normalizer::normalize_trace(trace).ok());
                    trace_utils::compute_top_level_span(trace);
                }
                input
            },
            BatchSize::LargeInput,
        );
    });
    // 2 compiles per root span, not a per-span cost.
    g.throughput(Throughput::Elements(2 * N_TRACES as u64));
    g.bench_function("regex_compile_per_root_span", |b| {
        b.iter(compile_filters_per_root_span)
    });
    g.finish();
}

/// Review 5.3: `span_matches_tag_regex` (trace_processor.rs) compiles its pattern on
/// every call, once per root span and filter.
#[allow(clippy::regex_creation_in_loops)]
fn compile_filters_per_root_span() {
    for _ in 0..N_TRACES {
        black_box(regex::Regex::new("^my-.*$").expect("regex"));
        black_box(regex::Regex::new(".*healthcheck.*").expect("regex"));
    }
}

fn stats(c: &mut Criterion) {
    common::setup_env();
    let rt = common::runtime();
    let config = make_config(|c| c.ext.lambda_extension_compute_stats = true);
    // The concentrator keeps a time window and drops spans outside it, so these start now.
    let traces = fixtures::traces::make_traces_at(fixtures::traces::now_ns(), N_TRACES, 20, true);
    let body_len = fixtures::traces::v04_msgpack(&traces).len();
    let (_, stats) = process(&config, traces, body_len);
    let stats = Arc::new(stats);

    let (service, handle) = StatsConcentratorService::new(config);
    rt.spawn(service.run());
    let generator = Arc::new(StatsGenerator::new(handle.clone()));

    let mut g = group(c);
    // Review 5.17: one boxed span clone and one channel send per span.
    for (name, include_drain) in [("generator_send", false), ("send_and_drain", true)] {
        g.bench_function(format!("stats/{name}"), |b| {
            b.iter_custom(|iters| {
                let (generator, handle, stats) = (generator.clone(), handle.clone(), stats.clone());
                rt.block_on(rt.spawn(async move {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let start = Instant::now();
                        generator.send(&stats).expect("stats send");
                        let sent = start.elapsed();
                        // The flush reply comes after every queued add is aggregated.
                        handle.flush(false).await.expect("stats flush");
                        total += if include_drain { start.elapsed() } else { sent };
                    }
                    total
                }))
                .expect("stats task")
            });
        });
    }
    g.finish();
}

#[derive(Clone, Debug)]
struct FakeClient;

impl HttpClientCapability for FakeClient {
    fn new_client() -> Self {
        FakeClient
    }

    fn new_without_connection_pooling() -> Self {
        FakeClient
    }

    fn request(
        &self,
        req: http::Request<bytes::Bytes>,
    ) -> impl Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend {
        black_box(req.body().len());
        async {
            Ok(http::Response::builder()
                .status(200)
                .body(bytes::Bytes::new())
                .expect("build response"))
        }
    }
}

impl SleepCapability for FakeClient {
    fn new() -> Self {
        FakeClient
    }

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + MaybeSend {
        tokio::time::sleep(duration)
    }
}

/// `AgentPayload` encoded straight from the borrowed payloads; byte-identical to libdd's.
fn encode_agent_payload_borrowed(tps: &[pb::TracerPayload]) -> Vec<u8> {
    use prost::encoding::{double, message};
    let len = tps
        .iter()
        .map(|tp| message::encoded_len(5, tp))
        .sum::<usize>()
        + double::encoded_len(8, &60.0)
        + double::encoded_len(9, &60.0);
    let mut buf = Vec::with_capacity(len);
    for tp in tps {
        message::encode(5, tp, &mut buf);
    }
    double::encode(8, &60.0, &mut buf);
    double::encode(9, &60.0, &mut buf);
    buf
}

/// libdd `construct_agent_payload`.
fn agent_payload(tracer_payloads: Vec<pb::TracerPayload>) -> pb::AgentPayload {
    pb::AgentPayload {
        host_name: String::new(),
        env: String::new(),
        agent_version: String::new(),
        error_tps: 60.0,
        target_tps: 60.0,
        tags: HashMap::new(),
        tracer_payloads,
        rare_sampler_enabled: false,
        idx_tracer_payloads: Vec::new(),
    }
}

/// libdd `serialize_proto_payload`.
fn encode(agent: &pb::AgentPayload) -> Vec<u8> {
    let mut buf = Vec::with_capacity(agent.encoded_len());
    agent.encode(&mut buf).expect("encode AgentPayload");
    buf
}

fn flush(c: &mut Criterion) {
    common::setup_env();
    let rt = common::runtime();
    let config = make_config(|_| {});
    let traces = fixtures::traces::trace_2000_spans();
    let body_len = fixtures::traces::v04_msgpack(&traces).len();
    let (info, _) = process(&config, traces, body_len);
    // As `TraceFlusher::flush` in src/traces/trace_flusher.rs, with one attempt and no backoff.
    let send_data = info
        .expect("trace payload")
        .builder
        .with_api_key("0123456789abcdef0123456789abcdef")
        .with_retry_strategy(RetryStrategy::new(1, 0, RetryBackoffType::Constant, None))
        .build();
    let TracerPayloadCollection::V07(tps) = send_data.get_payloads() else {
        unreachable!("process_traces returns V07")
    };
    let tps = tps.clone();
    let agent = agent_payload(tps.clone());
    let encoded = encode(&agent);
    assert_eq!(
        encode_agent_payload_borrowed(&tps),
        encoded,
        "borrowed encode must be byte-identical"
    );
    let sent = rt.block_on(send_data.send(&FakeClient));
    assert_eq!(sent.requests_count, 1, "one request per send");
    println!(
        "flush: protobuf {} bytes, zstd 1 {} bytes, zstd 3 {} bytes, on wire {} bytes (level {})",
        encoded.len(),
        zstd::bulk::compress(&encoded, 1).expect("zstd").len(),
        zstd::bulk::compress(&encoded, 3).expect("zstd").len(),
        sent.bytes_sent,
        config.apm_config_compression_level,
    );

    let mut g = group(c);
    // Review 5.4, 5.11: payload clone, protobuf encode and zstd in libdd `send_with_protobuf`.
    g.bench_function("flush/send_data_fake_http", |b| {
        b.to_async(&rt)
            .iter(|| async { send_data.send(&FakeClient).await });
    });
    // libdd `SendData::send_with_protobuf`
    g.bench_function("flush/payload_to_vec", |b| {
        b.iter_with_large_drop(|| tps.to_vec())
    });
    // From here on the unit of work is the encoded protobuf, not the spans.
    g.throughput(Throughput::Bytes(encoded.len() as u64));
    g.bench_function("flush/prost_encode_agent_payload", |b| {
        b.iter_with_large_drop(|| encode(&agent));
    });
    g.bench_function("flush/encode/current", |b| {
        b.iter_with_large_drop(|| encode(&agent_payload(tps.to_vec())));
    });
    g.bench_function("flush/encode/borrowed_manual", |b| {
        b.iter_with_large_drop(|| encode_agent_payload_borrowed(&tps));
    });
    for level in [1, 3] {
        g.bench_function(format!("flush/zstd/{level}"), |b| {
            b.iter_with_large_drop(|| zstd::bulk::compress(&encoded, level).expect("zstd"));
        });
    }
    g.finish();
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = decode, dedup, process_traces, parts, stats, flush
}
criterion_main!(benches);
