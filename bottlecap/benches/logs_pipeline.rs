// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Logs pipeline benchmark, used as the performance baseline for the migration to
//! `datadog-logs-agent`. Compare runs with `--save-baseline` / `--baseline`.

// Its `self_tests` module is unused when clippy checks this bench in test mode.
#[allow(unused_imports)]
#[path = "../tests/common/fake_logs_intake.rs"]
mod fake_logs_intake;

use std::collections::HashMap;
use std::sync::Arc;

use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::Config;
use bottlecap::config::logs_additional_endpoints::LogsAdditionalEndpoint;
use bottlecap::event_bus::EventBus;
use bottlecap::extension::telemetry::events::TelemetryEvent;
use bottlecap::logs::pipeline::{LogsPipeline, build_logs_pipeline};
use bottlecap::tags::provider::Provider;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use dogstatsd::api_key::ApiKeyFactory;
use fake_logs_intake::FakeLogsIntake;
use serde_json::json;

// Same allocator as the extension binary.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const ARN: &str = "arn:aws:lambda:us-east-1:123456789012:function:bench";
const INIT_START: &str = r#"{"time":"2022-10-21T14:05:03.000Z","type":"platform.initStart","record":{"initializationType":"on-demand","phase":"init"}}"#;
const WORDS: [&str; 16] = [
    "request", "handler", "cache", "miss", "user", "order", "payment", "retry", "timeout", "queue",
    "db", "query", "ok", "error", "shard", "session",
];

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

/// Must be called inside the runtime (the pipeline spawns its aggregator task).
fn build_pipeline(config: Config, is_managed_instance_mode: bool) -> (LogsPipeline, EventBus) {
    let config = Arc::new(config);
    let tags_provider = Arc::new(Provider::new(
        Arc::clone(&config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([("function_arn".to_string(), ARN.to_string())]),
    ));
    let (event_bus, event_bus_tx) = EventBus::run();
    // Pooled so loopback connections are reused: a fresh connection per request exhausts
    // ephemeral ports within a criterion run, and connection setup isn't what the
    // migration changes.
    let client = datadog_fips::reqwest_adapter::create_reqwest_client_builder()
        .expect("client builder")
        .danger_accept_invalid_certs(true)
        .build()
        .expect("client");
    let pipeline = build_logs_pipeline(
        &config,
        Arc::new(ApiKeyFactory::new("bench-key")),
        &tags_provider,
        event_bus_tx,
        is_managed_instance_mode,
        &client,
    );
    (pipeline, event_bus)
}

fn config(primary: &FakeLogsIntake, extras: &[FakeLogsIntake]) -> Config {
    Config {
        http_protocol: Some("http1".to_string()),
        skip_ssl_validation: true,
        logs_config_logs_dd_url: primary.base_url(),
        logs_config_use_compression: true,
        logs_config_compression_level: 3,
        logs_config_additional_endpoints: extras
            .iter()
            .map(|e| LogsAdditionalEndpoint {
                api_key: "extra-key".to_string(),
                host: "127.0.0.1".to_string(),
                port: u32::from(e.port()),
                is_reliable: true,
            })
            .collect(),
        ..Config::default()
    }
}

/// Varied but deterministic, so a batch compresses about 9:1 with zstd level 3.
fn log_message(i: usize) -> String {
    let h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let span = (h ^ (h >> 31)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let user = (span ^ (span >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    let w = |shift: u32| WORDS[usize::try_from((h >> shift) & 0xF).expect("word index")];
    format!(
        "log line {i:08} trace={h:016x} span={span:016x} user={user:016x} {} {} {} {} latency_ms={} bytes={}",
        w(0),
        w(4),
        w(8),
        w(12),
        h % 5_000,
        user % 100_000
    )
}

async fn process_all(pipeline: &mut LogsPipeline, events: &[TelemetryEvent]) -> Vec<Vec<u8>> {
    for event in events {
        pipeline
            .telemetry_tx
            .send(event.clone())
            .await
            .expect("send");
        pipeline.agent.sync_consume().await;
    }
    // HARNESS: aggregator-specific.
    pipeline
        .aggregator_handle
        .get_batches()
        .await
        .expect("batches")
}

async fn flush_once(pipeline: &LogsPipeline, batch: &[String]) {
    // HARNESS: aggregator-specific.
    pipeline
        .aggregator_handle
        .insert_batch(batch.to_vec())
        .expect("insert");
    // HARNESS: flusher-specific.
    assert!(pipeline.flusher.flush(None).await.is_empty());
}

/// Telemetry event → processor → aggregator → batches.
fn bench_process_and_batch(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("logs_process_and_batch");
    for (count, size) in [(1_000_usize, 200_usize), (10_000, 200), (1_000, 4_096)] {
        let events: Vec<TelemetryEvent> = (0..count)
            .map(|i| {
                serde_json::from_value(json!({
                    "time": "2022-10-21T14:05:04.000Z",
                    "type": "function",
                    "record": format!("{i:08} {}", "x".repeat(size))
                }))
                .expect("event")
            })
            .collect();
        let (mut pipeline, _event_bus, _intake) = rt.block_on(async {
            let intake = FakeLogsIntake::start_http().await;
            // Managed Instance mode ships logs without a request_id immediately.
            let (mut pipeline, event_bus) = build_pipeline(config(&intake, &[]), true);
            let init: TelemetryEvent = serde_json::from_str(INIT_START).expect("init");
            pipeline.telemetry_tx.send(init).await.expect("send");
            pipeline.agent.sync_consume().await;
            // HARNESS: aggregator-specific.
            let _ = pipeline.aggregator_handle.get_batches().await;
            (pipeline, event_bus, intake)
        });

        let batches = rt.block_on(process_all(&mut pipeline, &events));
        let total: usize = batches
            .iter()
            .map(|b| {
                serde_json::from_slice::<Vec<serde_json::Value>>(b)
                    .expect("batch")
                    .len()
            })
            .sum();
        assert_eq!(total, count, "every event should reach a batch");

        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{count}x{size}B")),
            &events,
            |b, events| {
                b.iter(|| rt.block_on(process_all(&mut pipeline, events)));
            },
        );
    }
    group.finish();
}

/// Aggregator → batch → zstd → HTTP, to the primary and 0 or 2 additional endpoints.
fn bench_flush(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("logs_flush");
    let batch: Vec<String> = (0..1_000)
        .map(|i| {
            json!({
                "message": {
                    "message": log_message(i),
                    "lambda": {"arn": ARN, "request_id": "459921b5-681c-4a96-beb0-81e0aa586026"},
                    "timestamp": 1_666_361_104_000_i64,
                    "status": "info"
                },
                "hostname": ARN,
                "service": "bench",
                "ddtags": "env:bench",
                "ddsource": "lambda"
            })
            .to_string()
        })
        .collect();

    for extras_count in [0_usize, 2] {
        let (pipeline, _event_bus, primary, extras) = rt.block_on(async {
            let primary = FakeLogsIntake::start_http().await;
            let mut extras = Vec::new();
            for _ in 0..extras_count {
                extras.push(FakeLogsIntake::start_https().await);
            }
            let (pipeline, event_bus) = build_pipeline(config(&primary, &extras), false);
            (pipeline, event_bus, primary, extras)
        });

        rt.block_on(flush_once(&pipeline, &batch));
        for intake in std::iter::once(&primary).chain(&extras) {
            let requests = intake.take_requests();
            assert_eq!(requests.len(), 1, "one request per endpoint");
            let logs = requests[0].json();
            assert_eq!(logs.as_array().expect("array").len(), batch.len());
        }

        group.throughput(Throughput::Elements(batch.len() as u64));
        group.bench_function(
            BenchmarkId::from_parameter(format!("1000_logs_{extras_count}_extras")),
            |b| {
                b.iter(|| {
                    rt.block_on(async {
                        flush_once(&pipeline, &batch).await;
                        primary.take_requests();
                        for extra in &extras {
                            extra.take_requests();
                        }
                    });
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_process_and_batch, bench_flush);
criterion_main!(benches);
