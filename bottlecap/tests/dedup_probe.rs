// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Real-intake dedup probe, with manual metrics readback.
//!
//! Sends synthetic stats payloads to Datadog's real intake to investigate
//! deduplication of retried `POST /api/v0.2/stats` requests. It verifies HTTP
//! acknowledgements, not resulting hit counts; poll the printed metrics query
//! separately. The three arms share a bucket timestamp, but network delays can
//! cross bucket boundaries and must be considered when interpreting results.
//! Arms use distinct runtime identities to avoid cross-arm deduplication.
//!
//! - control: 3 payloads with distinct resource names -> 3 series x k hits
//!   proves ingestion and readback work; without it a null result is
//!   uninterpretable
//! - test: 3 byte-identical payloads -> 1 series, 3xk hits if no dedup
//! - sequence variant: 3 payloads identical except incrementing `sequence`
//!   -> tests whether changing sequence affects the observed hit count
//!
//! Run with:
//!   DEDUP_PROBE_API_KEY=... cargo test --test dedup_probe -- --ignored --nocapture
//!
//! The key is read from the environment and never printed. Every send must
//! return 202; non-202 sends are logged and abort the arm.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use libdd_trace_protobuf::pb;
use libdd_trace_utils::stats_utils;

const SITE_TRACE_INTAKE: &str = "https://trace.agent.datadoghq.com/api/v0.2/stats";
const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Hits per payload. Larger k makes the readback unambiguous (30 vs 10).
const HITS_PER_PAYLOAD: u64 = 10;
const N: u64 = 3;

fn hex_id(len: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| format!("{:x}", rng.r#gen::<u8>() & 0xf))
        .collect()
}

/// Build one ClientStatsPayload. `resource` and `sequence` are the per-arm
/// knobs; runtime identity is held fixed within each retry or sequence arm.
fn synthetic_payload(
    service: &str,
    runtime_id: &str,
    resource: &str,
    sequence: u64,
    bucket_start_ns: u64,
) -> pb::ClientStatsPayload {
    pb::ClientStatsPayload {
        hostname: String::new(),
        env: "dedup-probe".to_string(),
        version: "probe".to_string(),
        lang: "rust-probe".to_string(),
        tracer_version: "probe".to_string(),
        runtime_id: runtime_id.to_string(),
        sequence,
        service: service.to_string(),
        stats: vec![pb::ClientStatsBucket {
            start: bucket_start_ns,
            duration: 10_000_000_000,
            stats: vec![pb::ClientGroupedStats {
                service: service.to_string(),
                name: "probe.request".to_string(),
                resource: resource.to_string(),
                r#type: "web".to_string(),
                http_status_code: 200,
                hits: HITS_PER_PAYLOAD,
                errors: 0,
                duration: 1_000_000,
                top_level_hits: HITS_PER_PAYLOAD,
                ..pb::ClientGroupedStats::default()
            }],
            ..pb::ClientStatsBucket::default()
        }],
        ..pb::ClientStatsPayload::default()
    }
}

#[test]
fn sequence_variants_only_change_sequence() {
    let first = synthetic_payload("service", "runtime", "resource", 1, 0);
    let mut second = synthetic_payload("service", "runtime", "resource", 2, 0);
    assert_eq!(first.runtime_id, second.runtime_id);
    assert_ne!(first.sequence, second.sequence);
    second.sequence = first.sequence;
    assert_eq!(first, second);
}

async fn send(api_key: &str, bytes: Vec<u8>, label: &str) {
    let client = reqwest::Client::new();
    let resp = client
        .post(SITE_TRACE_INTAKE)
        .header("Content-Type", "application/msgpack")
        .header("Content-Encoding", "gzip")
        .header("DD-API-KEY", api_key)
        .timeout(SEND_TIMEOUT)
        .body(bytes)
        .send()
        .await
        .expect("probe: send failed");
    let status = resp.status();
    println!("{label}: status={status}");
    assert!(
        status.as_u16() == 202,
        "{label}: real intake did not acknowledge (status {status}); this send is not part of the test"
    );
}

/// Bucket start truncated to the current 10s boundary in nanoseconds.
fn current_bucket_start_ns() -> u64 {
    let now_ns = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos(),
    )
    .expect("timestamp must fit in u64");
    now_ns - (now_ns % 10_000_000_000)
}

#[tokio::test]
#[ignore = "sends synthetic stats to the real Datadog intake; run explicitly with --ignored"]
async fn real_intake_dedup_probe() {
    let api_key = std::env::var("DEDUP_PROBE_API_KEY").expect("DEDUP_PROBE_API_KEY must be set");
    assert!(!api_key.is_empty(), "DEDUP_PROBE_API_KEY is empty");

    let service = format!("dedup-probe-{}", hex_id(12));
    let runtime_id = format!("probe-{}", hex_id(16));
    let bucket_start = current_bucket_start_ns();
    println!("probe service: {service}");
    println!("bucket start (ns): {bucket_start}");

    // Serialize up front. Only the identical-retry arm reuses the same bytes;
    // the sequence arm changes sequence while holding runtime identity fixed.
    let control_payloads: Vec<Vec<u8>> = (1..=N)
        .map(|i| {
            stats_utils::serialize_stats_payload(stats_utils::construct_stats_payload(vec![
                synthetic_payload(
                    &service,
                    &format!("{runtime_id}-control-{i}"),
                    &format!("control-{i}"),
                    1,
                    bucket_start,
                ),
            ]))
            .expect("probe: serialize failed")
        })
        .collect();
    let test_payload =
        stats_utils::serialize_stats_payload(stats_utils::construct_stats_payload(vec![
            synthetic_payload(
                &service,
                &format!("{runtime_id}-retry"),
                "dedup-test",
                1,
                bucket_start,
            ),
        ]))
        .expect("probe: serialize failed");
    let seq_payloads: Vec<Vec<u8>> = (1..=N)
        .map(|s| {
            stats_utils::serialize_stats_payload(stats_utils::construct_stats_payload(vec![
                synthetic_payload(
                    &service,
                    &format!("{runtime_id}-sequence"),
                    "dedup-seq",
                    s,
                    bucket_start,
                ),
            ]))
            .expect("probe: serialize failed")
        })
        .collect();

    println!("\n=== control arm: 3 distinct resources, sent once each ===");
    for (i, payload) in control_payloads.iter().enumerate() {
        send(
            &api_key,
            payload.clone(),
            &format!("control-{}/{}", i + 1, N),
        )
        .await;
    }

    println!("\n=== test arm: 3 byte-identical payloads ===");
    for i in 1..=N {
        send(&api_key, test_payload.clone(), &format!("test-{i}/{N}")).await;
    }

    println!("\n=== sequence arm: 3 payloads, incrementing sequence ===");
    for (i, payload) in seq_payloads.iter().enumerate() {
        send(
            &api_key,
            payload.clone(),
            &format!("sequence-{}/{}", i + 1, N),
        )
        .await;
    }

    println!("\nReadback (poll, do not single-shot; lag is minutes):");
    println!(
        "  query: sum:trace.probe.request.hits{{service:{service}}} by {{resource_name}}.as_count()"
    );
    println!("  expectations if no dedup:");
    println!("    control-1..3 series: {HITS_PER_PAYLOAD} each");
    println!(
        "    dedup-test series:   {} (3x{HITS_PER_PAYLOAD})",
        HITS_PER_PAYLOAD * N
    );
    println!(
        "    dedup-seq series:    {} (3x{HITS_PER_PAYLOAD})",
        HITS_PER_PAYLOAD * N
    );
    println!("  dedup-test = {HITS_PER_PAYLOAD}: identical retries collapsed");
    println!(
        "  dedup-seq = {}: changing sequence avoided that collapse",
        HITS_PER_PAYLOAD * N
    );
    println!("  dedup-seq = {HITS_PER_PAYLOAD}: changing sequence did not avoid collapse");
    println!("  Interpret only after all three controls appear with {HITS_PER_PAYLOAD} hits each.");
}
