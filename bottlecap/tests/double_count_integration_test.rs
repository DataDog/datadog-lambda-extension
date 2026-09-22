// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Double-count experiment: does the extension re-send byte-identical stats
//! after an ambiguous accept, producing two counted payloads at the intake?
//!
//! This is the local reproduction of the suspected statsv2 e2e overshoot
//! mechanism (see the statsv2 trace-stats overshoot investigation doc): the
//! stats flusher retries any error with the same serialized bytes and no
//! status discrimination, and the Datadog intake does not deduplicate
//! retried `POST /api/v0.2/stats` requests. An ambiguous accept (the intake
//! receives and processes the payload, then the response is lost) therefore
//! should yield TWO captured payloads for one logical send.
//!
//! Each test runs the real pipeline end to end: the `bottlecap-test-mode`
//! binary (spawned as a subprocess) computes stats from an ingested trace,
//! flushes to an in-process `FakeIntake`, and the fake-intake injection mode
//! simulates one of three delivery outcomes for the first stats attempt:
//!
//! - baseline: normal 202 → exactly 1 capture
//! - 500 injection (safe rejection, retried) → exactly 1 capture
//! - accept-then-drop (ambiguous accept, retried) → exactly 2 captures

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use bottlecap::fake_intake::{FakeIntake, FakeIntakeOptions};
use libdd_trace_protobuf::pb;
use rmp_serde::to_vec_named;
use serde_json::Value;

const FLUSH_WAIT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Kills the spawned test-mode process when the guard is dropped, including
/// on assertion failures, so failed tests never leak a listener on the port.
struct TestModeProcess(Child);

impl Drop for TestModeProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reserve a free TCP port on 127.0.0.1. There is a small bind race after the
/// reservation is dropped; acceptable for a test harness.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("test: failed to reserve a port")
        .local_addr()
        .expect("test: failed to read reserved port")
        .port()
}

/// Build the minimal v0.4 trace payload used as the stats source: one trace
/// with one finished server span carrying a single hit.
fn trace_payload_bytes() -> Vec<u8> {
    let span = pb::Span {
        service: "smoke-svc".to_string(),
        name: "smoke.request".to_string(),
        resource: "GET /smoke".to_string(),
        r#type: "web".to_string(),
        trace_id: 1_234_567_890_123,
        span_id: 987_654_321,
        start: 1,
        duration: 15_000_000,
        error: 0,
        ..pb::Span::default()
    };
    to_vec_named(&vec![vec![span]]).expect("test: msgpack encode failed")
}

/// Start the fake intake with the arm's injection options and a dump dir, and
/// spawn `bottlecap-test-mode` pointed at it with extension-side stats on.
async fn start_harness(options: FakeIntakeOptions) -> (FakeIntake, TestModeProcess, PathBuf) {
    let dump_dir = std::env::temp_dir().join(format!(
        "double-count-{}-{}",
        std::process::id(),
        free_port()
    ));
    let intake = FakeIntake::start_with_options(FakeIntakeOptions {
        dump_dir: Some(dump_dir.clone()),
        ..options
    })
    .await
    .expect("test: fake intake failed to start");

    let receiver_port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_bottlecap-test-mode"))
        .env("DD_APM_DD_URL", intake.base_url())
        .env("DD_APM_RECEIVER_PORT", receiver_port.to_string())
        .env("DD_LAMBDA_EXTENSION_COMPUTE_STATS", "true")
        .env("DD_SERVICE", "fake-intake-smoke")
        .env("DD_ENV", "local")
        .env("DD_VERSION", "smoke")
        // Be explicit about the manual flush strategy regardless of ambient env.
        .env_remove("DD_SERVERLESS_FLUSH_STRATEGY")
        .env_remove("DD_LOG_LEVEL")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("test: failed to spawn bottlecap-test-mode");
    let process = TestModeProcess(child);
    let receiver = format!("http://127.0.0.1:{receiver_port}");

    // Wait for the trace agent listener to answer /info.
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + FLUSH_WAIT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            panic!("test-mode did not answer /info in time");
        }
        if client
            .get(format!("{receiver}/info"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Ingest one trace: extension-side stats compute one stats payload for it.
    let body = trace_payload_bytes();
    let response = client
        .post(format!("{receiver}/v0.4/traces"))
        .header("Content-Type", "application/msgpack")
        .header("X-Datadog-Trace-Count", "1")
        .body(body)
        .send()
        .await
        .expect("test: trace ingest request failed");
    assert!(
        response.status().is_success(),
        "trace ingest failed: {}",
        response.status()
    );

    // Deterministic flush: ingest barrier guarantees the trace reached the
    // aggregator before /flush runs, and the serialized flush lock keeps
    // periodic drivers out of the way.
    let flush = client
        .post(format!("{receiver}/flush"))
        .send()
        .await
        .expect("test: /flush request failed");
    assert!(
        flush.status().as_u16() == 204 || flush.status().as_u16() == 502,
        "unexpected /flush status {}",
        flush.status()
    );

    (intake, process, dump_dir)
}

/// Poll the intake until at least `expected` stats payloads are captured, or
/// fail after the deadline. Returns all captured payloads.
async fn wait_for_stats(intake: &FakeIntake, expected: usize) -> Vec<pb::StatsPayload> {
    let deadline = tokio::time::Instant::now() + FLUSH_WAIT;
    loop {
        let captured = intake.stats_payloads();
        if captured.len() >= expected {
            return captured;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "expected {expected} stats captures, got {} in time",
                captured.len()
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn dump_files(dump_dir: &std::path::Path) -> Vec<Value> {
    let mut paths: Vec<_> = std::fs::read_dir(dump_dir)
        .expect("test: read dump dir failed")
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("_api_v0.2_stats-"))
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let raw = std::fs::read(&p).expect("test: read dump failed");
            serde_json::from_slice(&raw).expect("dump envelope must parse as JSON")
        })
        .collect()
}

#[tokio::test]
async fn baseline_delivers_exactly_one_stats_payload() {
    let (intake, _process, dump_dir) = start_harness(FakeIntakeOptions::default()).await;
    let captured = wait_for_stats(&intake, 1).await;
    assert_eq!(captured.len(), 1, "no injection: exactly one arrival");
    assert_eq!(
        dump_files(&dump_dir).len(),
        1,
        "no injection: exactly one stats dump"
    );
}

#[tokio::test]
async fn safe_rejection_retries_but_does_not_double_count() {
    let (intake, _process, dump_dir) = start_harness(FakeIntakeOptions {
        fail_stats_first_n: 1,
        ..FakeIntakeOptions::default()
    })
    .await;
    let captured = wait_for_stats(&intake, 1).await;
    assert_eq!(
        captured.len(),
        1,
        "500 rejection is retried, but rejected attempts are not captured"
    );
    // Two dumps prove the retry actually happened (attempt 1 rejected with
    // 500, attempt 2 accepted with 202); only the accepted one is captured.
    let dumps = dump_files(&dump_dir);
    assert_eq!(dumps.len(), 2, "one dump per attempt: rejection + retry");
    assert_eq!(dumps[0]["status"].as_u64(), Some(500));
    assert_eq!(dumps[1]["status"].as_u64(), Some(202));
}

#[tokio::test]
async fn ambiguous_accept_is_retried_and_double_counts() {
    let (intake, _process, dump_dir) = start_harness(FakeIntakeOptions {
        drop_stats_response_first_n: 1,
        ..FakeIntakeOptions::default()
    })
    .await;
    let captured = wait_for_stats(&intake, 2).await;
    assert_eq!(
        captured.len(),
        2,
        "the dropped attempt was accepted AND the retry was accepted: \
         the intake counted the same stats twice for one logical send"
    );
    assert_eq!(
        captured[0], captured[1],
        "the retried payload must be identical to the dropped one"
    );

    let dumps = dump_files(&dump_dir);
    assert_eq!(dumps.len(), 2);
    assert_eq!(dumps[0]["status"].as_u64(), Some(202));
    assert_eq!(
        dumps[0]["payload"]["dropped_after_accept"],
        Value::Bool(true),
        "first attempt must carry the ambiguous-accept marker"
    );
    assert!(
        dumps[1]["payload"]
            .as_object()
            .expect("payload must be an object")
            .get("dropped_after_accept")
            .is_none(),
        "the retried attempt must not carry the drop marker"
    );
    // Byte-identity check on the wire-relevant content: strip the marker from
    // the dropped attempt's payload and compare with the retry's payload.
    let mut first = dumps[0]["payload"].clone();
    first
        .as_object_mut()
        .expect("payload must be an object")
        .remove("dropped_after_accept");
    assert_eq!(
        first, dumps[1]["payload"],
        "the retry must resend byte-identical stats content"
    );
}
