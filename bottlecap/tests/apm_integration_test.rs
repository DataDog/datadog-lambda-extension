// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Payload-level APM integration tests using the in-process fake-intake.
//!
//! Covers the two flush paths bottlecap uses to forward APM data to the
//! Datadog backend:
//!
//! - `StatsFlusher` → msgpack+gzip `pb::StatsPayload` on `/api/v0.2/stats`
//! - `TraceFlusher` → protobuf `pb::AgentPayload` on `/api/v0.2/traces`
//!
//! Each test spins up a `FakeIntake`, points the flusher at it, triggers a
//! flush, then decodes the captured payload and asserts on concrete fields.
//! This is what APMSVLS-496 phase 1 unblocks: regression coverage for
//! payload-level changes that `body_contains`-style mocks can't catch.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::Config;
use bottlecap::tags::provider::Provider;
use bottlecap::traces::http_client::create_client;
use bottlecap::traces::stats_aggregator::StatsAggregator;
use bottlecap::traces::stats_concentrator_service::StatsConcentratorService;
use bottlecap::traces::stats_flusher::StatsFlusher;
use bottlecap::traces::stats_generator::StatsGenerator;
use bottlecap::traces::trace_aggregator::SendDataBuilderInfo;
use bottlecap::traces::trace_aggregator_service::AggregatorService;
use bottlecap::traces::trace_flusher::TraceFlusher;
use bottlecap::traces::trace_processor::{SendingTraceProcessor, ServerlessTraceProcessor};
use dogstatsd::api_key::ApiKeyFactory;
use libdd_common::Endpoint;
use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::send_data::SendDataBuilder;
use libdd_trace_utils::trace_utils::TracerHeaderTags;
use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
use tokio::sync::Mutex;

#[path = "common/fake_intake.rs"]
mod fake_intake;

use fake_intake::FakeIntake;

const DD_API_KEY: &str = "my_test_key";

fn header_tags() -> TracerHeaderTags<'static> {
    TracerHeaderTags {
        lang: "rust",
        lang_version: "1.80",
        lang_interpreter: "rustc",
        lang_vendor: "datadog",
        tracer_version: "test",
        container_id: "",
        client_computed_top_level: true,
        client_computed_stats: true,
        dropped_p0_traces: 0,
        dropped_p0_spans: 0,
    }
}

fn test_config() -> Arc<Config> {
    Arc::new(Config {
        api_key: DD_API_KEY.to_string(),
        site: "datadoghq.com".to_string(),
        ..Config::default()
    })
}

fn endpoint_for(url: &str, api_key: &str) -> Endpoint {
    Endpoint {
        url: hyper::Uri::from_str(url).expect("test endpoint URL must parse"),
        api_key: Some(api_key.to_string().into()),
        timeout_ms: 5_000,
        test_token: None,
        use_system_resolver: false,
    }
}

#[tokio::test]
async fn stats_payload_roundtrip_through_fake_intake() {
    let fake_intake = FakeIntake::start().await;
    let config = test_config();
    let http_client = create_client(None, None, false).expect("failed to create http client");

    // StatsFlusher::send() works directly on a Vec<ClientStatsPayload>,
    // bypassing the aggregator/concentrator. We still need to supply a
    // StatsAggregator because the struct holds one; an idle concentrator
    // is fine since send() never touches the aggregator.
    let (concentrator_service, concentrator_handle) =
        StatsConcentratorService::new(Arc::clone(&config));
    tokio::spawn(concentrator_service.run());
    let aggregator = Arc::new(Mutex::new(StatsAggregator::new_with_concentrator(
        concentrator_handle,
    )));

    let api_key_factory = Arc::new(ApiKeyFactory::new(DD_API_KEY));
    let flusher = StatsFlusher::new(
        api_key_factory,
        aggregator,
        config,
        http_client,
        fake_intake.stats_url(),
    );

    let client_stats = pb::ClientStatsPayload {
        hostname: "test-host".to_string(),
        env: "test-env".to_string(),
        version: "1.2.3".to_string(),
        lang: "rust".to_string(),
        tracer_version: "test-tracer".to_string(),
        runtime_id: "00000000-0000-0000-0000-000000000001".to_string(),
        sequence: 7,
        service: "fake-intake-test-service".to_string(),
        stats: vec![pb::ClientStatsBucket {
            start: 1_700_000_000_000_000_000,
            duration: 10_000_000_000,
            agent_time_shift: 0,
            stats: vec![pb::ClientGroupedStats {
                service: "fake-intake-test-service".to_string(),
                name: "handler".to_string(),
                resource: "GET /fake".to_string(),
                r#type: "web".to_string(),
                http_status_code: 200,
                db_type: String::new(),
                hits: 3,
                errors: 0,
                duration: 42,
                ok_summary: vec![0, 0, 0],
                error_summary: vec![0, 0, 0],
                synthetics: false,
                top_level_hits: 3,
                span_kind: "server".to_string(),
                peer_tags: vec!["peer.service:upstream".to_string()],
                is_trace_root: pb::Trilean::True.into(),
                grpc_status_code: String::new(),
                http_endpoint: "/fake".to_string(),
                http_method: "GET".to_string(),
                service_source: String::new(),
                span_derived_primary_tags: vec![],
            }],
        }],
        agent_aggregation: String::new(),
        container_id: String::new(),
        tags: vec![],
        git_commit_sha: String::new(),
        image_tag: String::new(),
        process_tags_hash: 0,
        process_tags: String::new(),
    };

    let failed = flusher.send(vec![client_stats]).await;
    assert!(
        failed.is_none(),
        "stats send reported a retry-able failure: {failed:?}",
    );

    let captured = fake_intake.stats_payloads();
    assert_eq!(captured.len(), 1, "expected exactly one StatsPayload");

    let payload = &captured[0];
    assert!(
        payload.client_computed,
        "bottlecap is the agent; client_computed must be true",
    );
    assert_eq!(payload.stats.len(), 1);
    let inner = &payload.stats[0];
    // libdd_trace_utils::stats_utils::construct_stats_payload zeroes hostname on every
    // input before wrapping, so the sent value is "" regardless of what the caller set.
    assert_eq!(inner.hostname, "");
    assert_eq!(inner.env, "test-env");
    assert_eq!(inner.version, "1.2.3");
    assert_eq!(inner.service, "fake-intake-test-service");
    assert_eq!(inner.sequence, 7);
    assert_eq!(inner.stats.len(), 1);
    let bucket = &inner.stats[0];
    assert_eq!(bucket.stats.len(), 1);
    let grouped = &bucket.stats[0];
    assert_eq!(grouped.name, "handler");
    assert_eq!(grouped.resource, "GET /fake");
    assert_eq!(grouped.hits, 3);
    assert_eq!(grouped.top_level_hits, 3);
    assert_eq!(grouped.span_kind, "server");
    assert_eq!(grouped.peer_tags, vec!["peer.service:upstream".to_string()]);
    assert_eq!(grouped.http_status_code, 200);
    assert_eq!(grouped.http_method, "GET");
    assert_eq!(grouped.http_endpoint, "/fake");
    assert_eq!(grouped.is_trace_root, pb::Trilean::True as i32);
}

#[tokio::test]
async fn trace_payload_roundtrip_through_fake_intake() {
    let fake_intake = FakeIntake::start().await;
    let config = test_config();
    let http_client = create_client(None, None, false).expect("failed to create http client");
    let endpoint = endpoint_for(&fake_intake.traces_url(), DD_API_KEY);

    let (aggregator_service, aggregator_handle) = AggregatorService::default();
    tokio::spawn(aggregator_service.run());

    let span = pb::Span {
        service: "fake-intake-trace-service".to_string(),
        name: "web.request".to_string(),
        resource: "GET /fake".to_string(),
        trace_id: 0x1111_1111_1111_1111,
        span_id: 0x2222_2222_2222_2222,
        parent_id: 0,
        start: STATS_SPAN_START_NS,
        duration: 5_000_000,
        error: 0,
        r#type: "web".to_string(),
        ..pb::Span::default()
    };
    let chunk = pb::TraceChunk {
        priority: 1,
        origin: String::new(),
        spans: vec![span],
        tags: Default::default(),
        dropped_trace: false,
    };
    let tracer_payload = pb::TracerPayload {
        container_id: String::new(),
        language_name: "rust".to_string(),
        language_version: "1.80".to_string(),
        tracer_version: "test".to_string(),
        runtime_id: "00000000-0000-0000-0000-000000000002".to_string(),
        chunks: vec![chunk],
        tags: Default::default(),
        env: "test-env".to_string(),
        hostname: String::new(),
        app_version: "1.2.3".to_string(),
    };

    let tags = header_tags();
    let builder = SendDataBuilder::new(
        1,
        TracerPayloadCollection::V07(vec![tracer_payload]),
        tags.clone(),
        &endpoint,
    );
    aggregator_handle
        .insert_payload(SendDataBuilderInfo::new(builder, 1, tags.into()))
        .expect("insert_payload must succeed");

    let api_key_factory = Arc::new(ApiKeyFactory::new(DD_API_KEY));
    let flusher = TraceFlusher::new(aggregator_handle, config, api_key_factory, http_client);

    let failed = flusher.flush(None).await;
    assert!(
        failed.is_none(),
        "trace flush reported a retry-able failure: {failed:?}",
    );

    let captured = fake_intake.trace_payloads();
    assert_eq!(captured.len(), 1, "expected exactly one AgentPayload");

    let payload = &captured[0];
    assert_eq!(payload.tracer_payloads.len(), 1);
    let tp = &payload.tracer_payloads[0];
    assert_eq!(tp.language_name, "rust");
    assert_eq!(tp.env, "test-env");
    assert_eq!(tp.app_version, "1.2.3");
    assert_eq!(tp.chunks.len(), 1);
    let chunk = &tp.chunks[0];
    assert_eq!(chunk.priority, 1);
    assert_eq!(chunk.spans.len(), 1);
    let span = &chunk.spans[0];
    assert_eq!(span.service, "fake-intake-trace-service");
    assert_eq!(span.name, "web.request");
    assert_eq!(span.resource, "GET /fake");
    assert_eq!(span.trace_id, 0x1111_1111_1111_1111);
    assert_eq!(span.span_id, 0x2222_2222_2222_2222);
}

// ---------------------------------------------------------------------------
// APMSVLS-487 Tier 3: full fake-intake E2E through `SendingTraceProcessor`.
//
// Unlike `trace_payload_roundtrip_through_fake_intake` (which inserts a hand-built
// `pb::TracerPayload` directly), these tests route a trace through
// `SendingTraceProcessor::send_processed_traces` so they exercise the real
// `process_traces`/`ChunkProcessor` stamping of `_dd.compute_stats` and the
// extension-side stats-generation guard.
// ---------------------------------------------------------------------------

fn header_tags_with(client_computed_stats: bool) -> TracerHeaderTags<'static> {
    TracerHeaderTags {
        client_computed_stats,
        ..header_tags()
    }
}

/// Outcome of routing a single trace through the processor + flushers.
struct PipelineOutcome {
    traces: Vec<pb::AgentPayload>,
    stats: Vec<pb::StatsPayload>,
}

/// Drives `traces` through `SendingTraceProcessor::send_processed_traces` with the given
/// `compute_trace_stats_on_extension` / `client_computed_stats`, then flushes both the trace
/// and stats pipelines into a fresh fake-intake and returns what it captured.
async fn run_processor_pipeline_with_traces(
    compute_on_extension: bool,
    client_computed_stats: bool,
    traces: Vec<Vec<pb::Span>>,
) -> PipelineOutcome {
    let fake_intake = FakeIntake::start().await;

    let config = Arc::new(Config {
        api_key: DD_API_KEY.to_string(),
        site: "datadoghq.com".to_string(),
        // process_traces builds its trace endpoint directly from apm_dd_url.
        apm_dd_url: fake_intake.traces_url(),
        service: Some("fake-intake-trace-service".to_string()),
        ext: bottlecap::config::LambdaConfig {
            compute_trace_stats_on_extension: compute_on_extension,
            ..Default::default()
        },
        ..Config::default()
    });

    // --- Trace pipeline: trace_tx -> (drained below) -> AggregatorService -> TraceFlusher ---
    let (aggregator_service, aggregator_handle) = AggregatorService::default();
    tokio::spawn(aggregator_service.run());
    let (trace_tx, mut trace_rx) = tokio::sync::mpsc::channel::<SendDataBuilderInfo>(8);

    // --- Stats pipeline: StatsConcentratorService -> StatsAggregator -> StatsFlusher ---
    let (concentrator_service, concentrator_handle) =
        StatsConcentratorService::new(Arc::clone(&config));
    tokio::spawn(concentrator_service.run());

    let sender = SendingTraceProcessor {
        appsec: None,
        processor: Arc::new(ServerlessTraceProcessor {
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
        }),
        trace_tx,
        stats_generator: Arc::new(StatsGenerator::new(concentrator_handle.clone())),
    };

    let tags_provider = Arc::new(Provider::new(
        Arc::clone(&config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &std::collections::HashMap::from([(
            "function_arn".to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        )]),
    ));

    sender
        .send_processed_traces(
            Arc::clone(&config),
            tags_provider,
            header_tags_with(client_computed_stats),
            traces,
            100,
            None,
        )
        .await
        .expect("send_processed_traces failed");

    // Drain whatever `send_processed_traces` produced into the aggregator before flushing.
    // `send_processed_traces` has already returned, so the payload (if any) is buffered in the
    // channel and `try_recv` will surface it without racing a background task.
    drop(sender);
    while let Ok(info) = trace_rx.try_recv() {
        aggregator_handle
            .insert_payload(info)
            .expect("insert_payload must succeed");
    }

    // Flush traces.
    let http_client = create_client(None, None, false).expect("failed to create http client");
    let api_key_factory = Arc::new(ApiKeyFactory::new(DD_API_KEY));
    let trace_flusher = TraceFlusher::new(
        aggregator_handle,
        Arc::clone(&config),
        Arc::clone(&api_key_factory),
        http_client.clone(),
    );
    let failed = trace_flusher.flush(None).await;
    assert!(failed.is_none(), "trace flush failed: {failed:?}");

    // Flush stats (pulls from the concentrator via the aggregator).
    let stats_aggregator = Arc::new(Mutex::new(StatsAggregator::new_with_concentrator(
        concentrator_handle,
    )));
    let stats_flusher = StatsFlusher::new(
        api_key_factory,
        stats_aggregator,
        Arc::clone(&config),
        http_client,
        fake_intake.stats_url(),
    );
    let failed = stats_flusher.flush(true, None).await;
    assert!(failed.is_none(), "stats flush failed: {failed:?}");

    PipelineOutcome {
        traces: fake_intake.trace_payloads(),
        stats: fake_intake.stats_payloads(),
    }
}

/// Fixed span start well in the past relative to the concentrator's clock, so all spans
/// fold into the single oldest bucket and a `force_flush` returns exactly one bucket.
/// That keeps the aggregation assertions deterministic without controlling time.
const STATS_SPAN_START_NS: i64 = 1_700_000_000_000_000_000;

/// Single-root-span convenience wrapper used by the `_dd.compute_stats` tests below.
async fn run_processor_pipeline(
    compute_on_extension: bool,
    client_computed_stats: bool,
) -> PipelineOutcome {
    // A top-level root span so the concentrator produces stats.
    let mut span = pb::Span {
        service: "fake-intake-trace-service".to_string(),
        name: "web.request".to_string(),
        resource: "GET /fake".to_string(),
        trace_id: 0x1111_1111_1111_1111,
        span_id: 0x2222_2222_2222_2222,
        parent_id: 0,
        start: STATS_SPAN_START_NS,
        duration: 5_000_000,
        error: 0,
        r#type: "web".to_string(),
        ..pb::Span::default()
    };
    span.metrics.insert("_top_level".to_string(), 1.0);

    run_processor_pipeline_with_traces(
        compute_on_extension,
        client_computed_stats,
        vec![vec![span]],
    )
    .await
}

/// Finds the single span in the captured trace payloads and returns its `_dd.compute_stats`.
// Mirror of the crate-internal bottlecap::tags::lambda::tags::COMPUTE_STATS_KEY,
// duplicated here to keep that constant pub(crate).
const COMPUTE_STATS_KEY: &str = "_dd.compute_stats";

fn captured_compute_stats(traces: &[pb::AgentPayload]) -> Option<String> {
    let span = traces
        .iter()
        .flat_map(|p| &p.tracer_payloads)
        .flat_map(|tp| &tp.chunks)
        .flat_map(|c| &c.spans)
        .find(|s| s.name == "web.request")
        .expect("web.request span should be present");
    span.meta.get(COMPUTE_STATS_KEY).cloned()
}

/// T3.1: `client_computed_stats=true` → captured span meta has `_dd.compute_stats` absent.
#[tokio::test]
async fn e2e_client_computed_stats_leaves_compute_stats_absent() {
    let outcome = run_processor_pipeline(false, true).await;
    assert!(
        captured_compute_stats(&outcome.traces).is_none(),
        "_dd.compute_stats must be absent when the tracer computed stats",
    );
}

/// T3.2: control matrix — `"1"` only for the (neither computes) row; absent otherwise.
#[tokio::test]
async fn e2e_compute_stats_truth_table_on_captured_span() {
    let cases = [
        (false, false, Some("1")),
        (false, true, None),
        (true, false, None),
        (true, true, None),
    ];
    for (compute_on_extension, client_computed_stats, expected) in cases {
        let outcome = run_processor_pipeline(compute_on_extension, client_computed_stats).await;
        assert_eq!(
            captured_compute_stats(&outcome.traces).as_deref(),
            expected,
            "compute_on_extension={compute_on_extension}, client_computed_stats={client_computed_stats}",
        );
    }
}

/// T3.3: stats suppression — a stats payload is produced only when the extension computes
/// stats and the tracer did not; otherwise the stats intake stays empty.
#[tokio::test]
async fn e2e_stats_suppressed_unless_extension_computes() {
    // Extension computes, tracer didn't -> exactly one stats payload.
    let outcome = run_processor_pipeline(true, false).await;
    assert_eq!(
        outcome.stats.len(),
        1,
        "expected one stats payload when the extension computes stats",
    );

    // Tracer computed -> extension skips stats generation.
    let outcome = run_processor_pipeline(true, true).await;
    assert!(
        outcome.stats.is_empty(),
        "stats must be suppressed when the tracer computed them",
    );

    // Extension not computing -> no stats either way.
    let outcome = run_processor_pipeline(false, false).await;
    assert!(
        outcome.stats.is_empty(),
        "stats must be empty when the extension does not compute them",
    );
}

/// T3.4: combined path — when the tracer computed stats, the captured trace has no
/// `_dd.compute_stats` AND zero stats payloads reach the intake.
#[tokio::test]
async fn e2e_client_computed_stats_absent_meta_and_no_stats() {
    let outcome = run_processor_pipeline(true, true).await;
    assert!(
        captured_compute_stats(&outcome.traces).is_none(),
        "_dd.compute_stats must be absent",
    );
    assert!(outcome.stats.is_empty(), "no stats payloads must be sent",);
}

// ---------------------------------------------------------------------------
// Stats aggregation correctness: route concrete spans through the real
// SpanConcentrator and assert on the *computed* aggregate values (hits, errors,
// duration, grouping) that reach the intake, not just on stats presence.
//
// Determinism comes from `STATS_SPAN_START_NS` (see its doc comment).
// ---------------------------------------------------------------------------

/// Builds a single-span trace the concentrator will count toward stats.
/// `parent_id: 0` makes it a trace root and the `_top_level` metric makes the
/// concentrator include it (non-top-level, non-measured spans are otherwise skipped).
/// Distinct `id` values keep trace/span ids unique across invocations.
fn stats_trace(id: u64, resource: &str, duration: i64, error: i32) -> Vec<pb::Span> {
    let mut span = pb::Span {
        service: "fake-intake-trace-service".to_string(),
        name: "web.request".to_string(),
        resource: resource.to_string(),
        trace_id: id,
        span_id: id,
        parent_id: 0,
        start: STATS_SPAN_START_NS,
        duration,
        error,
        r#type: "web".to_string(),
        ..pb::Span::default()
    };
    span.metrics.insert("_top_level".to_string(), 1.0);
    vec![span]
}

/// Flattens every `ClientGroupedStats` across all buckets of the single captured stats payload.
fn captured_grouped_stats(stats: &[pb::StatsPayload]) -> Vec<pb::ClientGroupedStats> {
    assert_eq!(stats.len(), 1, "expected exactly one stats payload");
    stats[0]
        .stats
        .iter()
        .flat_map(|csp| &csp.stats)
        .flat_map(|bucket| &bucket.stats)
        .cloned()
        .collect()
}

/// AGG-1: N identical top-level spans collapse into one group with `hits == N`.
#[tokio::test]
async fn e2e_stats_count_aggregates_identical_spans() {
    let traces = vec![
        stats_trace(1, "GET /fake", 1_000_000, 0),
        stats_trace(2, "GET /fake", 1_000_000, 0),
        stats_trace(3, "GET /fake", 1_000_000, 0),
    ];
    let outcome = run_processor_pipeline_with_traces(true, false, traces).await;
    let grouped = captured_grouped_stats(&outcome.stats);
    assert_eq!(
        grouped.len(),
        1,
        "identical spans must collapse to one group"
    );
    assert_eq!(grouped[0].hits, 3);
    assert_eq!(grouped[0].top_level_hits, 3);
    assert_eq!(grouped[0].errors, 0);
}

/// AGG-2: `errors` counts only spans with `error != 0`; `hits` counts all of them.
#[tokio::test]
async fn e2e_stats_counts_errors_separately_from_hits() {
    let traces = vec![
        stats_trace(1, "GET /fake", 1_000_000, 0),
        stats_trace(2, "GET /fake", 1_000_000, 1),
        stats_trace(3, "GET /fake", 1_000_000, 1),
    ];
    let outcome = run_processor_pipeline_with_traces(true, false, traces).await;
    let grouped = captured_grouped_stats(&outcome.stats);
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].hits, 3);
    assert_eq!(grouped[0].errors, 2);
}

/// AGG-3: `duration` is the nanosecond sum of every aggregated span's duration.
#[tokio::test]
async fn e2e_stats_sums_span_durations() {
    let traces = vec![
        stats_trace(1, "GET /fake", 1_000_000, 0),
        stats_trace(2, "GET /fake", 2_000_000, 0),
        stats_trace(3, "GET /fake", 3_000_000, 0),
    ];
    let outcome = run_processor_pipeline_with_traces(true, false, traces).await;
    let grouped = captured_grouped_stats(&outcome.stats);
    assert_eq!(grouped.len(), 1);
    assert_eq!(grouped[0].hits, 3);
    assert_eq!(grouped[0].duration, 6_000_000);
}

/// AGG-4: spans with distinct resources stay in separate groups, each counted independently.
#[tokio::test]
async fn e2e_stats_groups_by_resource() {
    let traces = vec![
        stats_trace(1, "GET /a", 1_000_000, 0),
        stats_trace(2, "GET /a", 1_000_000, 0),
        stats_trace(3, "GET /b", 1_000_000, 0),
    ];
    let outcome = run_processor_pipeline_with_traces(true, false, traces).await;
    let grouped = captured_grouped_stats(&outcome.stats);
    assert_eq!(grouped.len(), 2, "distinct resources must not be merged");
    let a = grouped
        .iter()
        .find(|g| g.resource == "GET /a")
        .expect("GET /a group should be present");
    let b = grouped
        .iter()
        .find(|g| g.resource == "GET /b")
        .expect("GET /b group should be present");
    assert_eq!(a.hits, 2);
    assert_eq!(b.hits, 1);
}

/// Like `stats_trace` but stamps extra `meta` keys on the span (e.g. `span.kind`,
/// `http.status_code`) that the concentrator folds into the aggregation key.
fn stats_trace_with_meta(id: u64, resource: &str, meta: &[(&str, &str)]) -> Vec<pb::Span> {
    let mut trace = stats_trace(id, resource, 1_000_000, 0);
    for (key, value) in meta {
        trace[0]
            .meta
            .insert((*key).to_string(), (*value).to_string());
    }
    trace
}

/// AGG-5: spans differing only in `span.kind` stay in separate groups.
#[tokio::test]
async fn e2e_stats_groups_by_span_kind() {
    let traces = vec![
        stats_trace_with_meta(1, "GET /fake", &[("span.kind", "server")]),
        stats_trace_with_meta(2, "GET /fake", &[("span.kind", "server")]),
        stats_trace_with_meta(3, "GET /fake", &[("span.kind", "internal")]),
    ];
    let outcome = run_processor_pipeline_with_traces(true, false, traces).await;
    let grouped = captured_grouped_stats(&outcome.stats);
    assert_eq!(grouped.len(), 2, "distinct span.kind must not be merged");
    let server = grouped
        .iter()
        .find(|g| g.span_kind == "server")
        .expect("server group should be present");
    let internal = grouped
        .iter()
        .find(|g| g.span_kind == "internal")
        .expect("internal group should be present");
    assert_eq!(server.hits, 2);
    assert_eq!(internal.hits, 1);
}

/// AGG-6: spans differing only in `http.status_code` stay in separate groups.
#[tokio::test]
async fn e2e_stats_groups_by_http_status_code() {
    let traces = vec![
        stats_trace_with_meta(1, "GET /fake", &[("http.status_code", "200")]),
        stats_trace_with_meta(2, "GET /fake", &[("http.status_code", "200")]),
        stats_trace_with_meta(3, "GET /fake", &[("http.status_code", "500")]),
    ];
    let outcome = run_processor_pipeline_with_traces(true, false, traces).await;
    let grouped = captured_grouped_stats(&outcome.stats);
    assert_eq!(
        grouped.len(),
        2,
        "distinct http.status_code must not be merged"
    );
    let ok = grouped
        .iter()
        .find(|g| g.http_status_code == 200)
        .expect("200 group should be present");
    let err = grouped
        .iter()
        .find(|g| g.http_status_code == 500)
        .expect("500 group should be present");
    assert_eq!(ok.hits, 2);
    assert_eq!(err.hits, 1);
}

/// Build a non-root, non-measured span eligible for stats only via its `span.kind`.
///
/// `parent_id` is non-zero (non-root) and `metrics` is empty (no `_top_level` /
/// `_dd.measured`), so the concentrator will only compute stats for it when its
/// `span.kind` is in `span_kinds_stats_computed`. `start` is set to "now" so the
/// span lands in the current bucket and a forced flush returns it.
fn make_eligible_span(span_kind: &str, peer_meta: &[(&str, &str)]) -> pb::Span {
    let now_ns = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos(),
    )
    .expect("nanos since epoch must fit in i64");

    let mut meta: HashMap<String, String> = peer_meta
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    meta.insert("span.kind".to_string(), span_kind.to_string());

    pb::Span {
        service: "fake-intake-stats-service".to_string(),
        name: "test-op".to_string(),
        resource: "test-resource".to_string(),
        trace_id: 1,
        span_id: 2,
        parent_id: 1, // non-root
        start: now_ns,
        duration: 100,
        error: 0,
        r#type: "web".to_string(),
        meta,
        metrics: HashMap::new(), // no _top_level, no _dd.measured
        ..pb::Span::default()
    }
}

/// Wire concentrator -> aggregator -> flusher pointed at the fake intake, feed in
/// `spans`, force a flush, and return the single captured `StatsPayload`.
async fn flush_spans_to_fake_intake(
    fake_intake: &FakeIntake,
    spans: &[pb::Span],
) -> pb::StatsPayload {
    let config = test_config();
    let http_client = create_client(None, None, false).expect("failed to create http client");

    let (concentrator_service, concentrator_handle) =
        StatsConcentratorService::new(Arc::clone(&config));
    tokio::spawn(concentrator_service.run());

    let aggregator = Arc::new(Mutex::new(StatsAggregator::new_with_concentrator(
        concentrator_handle.clone(),
    )));

    for span in spans {
        concentrator_handle
            .add(span)
            .expect("concentrator add must succeed");
    }

    let api_key_factory = Arc::new(ApiKeyFactory::new(DD_API_KEY));
    let flusher = StatsFlusher::new(
        api_key_factory,
        aggregator,
        config,
        http_client,
        fake_intake.stats_url(),
    );

    let failed = flusher.flush(true, None).await;
    assert!(
        failed.is_none(),
        "stats flush reported a retry-able failure: {failed:?}",
    );

    let captured = fake_intake.stats_payloads();
    assert_eq!(captured.len(), 1, "expected exactly one StatsPayload");
    captured.into_iter().next().expect("captured payload")
}

/// End-to-end: a non-root, non-measured `span.kind="server"` span fed through the
/// concentrator must survive aggregation + msgpack/gzip serialization and arrive
/// at the intake as a grouped-stats entry with `span_kind="server"`. This closes
/// the gap left by the in-memory concentrator unit tests, which never serialize.
#[tokio::test]
async fn stats_span_kind_through_fake_intake() {
    let fake_intake = FakeIntake::start().await;
    let span = make_eligible_span("server", &[]);

    let payload = flush_spans_to_fake_intake(&fake_intake, &[span]).await;

    let grouped: Vec<_> = payload
        .stats
        .iter()
        .flat_map(|p| &p.stats)
        .flat_map(|b| &b.stats)
        .collect();
    assert!(
        !grouped.is_empty(),
        "expected at least one grouped-stats entry for the server span",
    );
    assert!(
        grouped.iter().any(|s| s.span_kind == "server"),
        "expected a grouped-stats entry with span_kind='server', got: {:?}",
        grouped.iter().map(|s| &s.span_kind).collect::<Vec<_>>(),
    );
}

/// End-to-end: a `span.kind="client"` span carrying peer-tag meta keys
/// (`db.instance`, `db.system`) must arrive at the intake with those keys
/// populated in `peer_tags`, proving peer-tags survive serialization through
/// the concentrator -> flusher -> intake path.
#[tokio::test]
async fn stats_peer_tags_through_fake_intake() {
    let fake_intake = FakeIntake::start().await;
    let span = make_eligible_span(
        "client",
        &[("db.instance", "i-1234"), ("db.system", "postgres")],
    );

    let payload = flush_spans_to_fake_intake(&fake_intake, &[span]).await;

    let with_peer_tags: Vec<_> = payload
        .stats
        .iter()
        .flat_map(|p| &p.stats)
        .flat_map(|b| &b.stats)
        .filter(|s| !s.peer_tags.is_empty())
        .collect();
    assert!(
        !with_peer_tags.is_empty(),
        "expected at least one grouped-stats entry with non-empty peer_tags",
    );
    let peer_tags = &with_peer_tags[0].peer_tags;
    assert!(
        peer_tags.iter().any(|t| t.starts_with("db.instance:")),
        "expected peer_tags to contain db.instance, got: {peer_tags:?}",
    );
    assert!(
        peer_tags.iter().any(|t| t.starts_with("db.system:")),
        "expected peer_tags to contain db.system, got: {peer_tags:?}",
    );
}
