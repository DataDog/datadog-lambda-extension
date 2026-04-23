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

use std::str::FromStr;
use std::sync::Arc;

use bottlecap::config::Config;
use bottlecap::traces::http_client::create_client;
use bottlecap::traces::stats_aggregator::StatsAggregator;
use bottlecap::traces::stats_concentrator_service::StatsConcentratorService;
use bottlecap::traces::stats_flusher::StatsFlusher;
use bottlecap::traces::trace_aggregator::SendDataBuilderInfo;
use bottlecap::traces::trace_aggregator_service::AggregatorService;
use bottlecap::traces::trace_flusher::TraceFlusher;
use dogstatsd::api_key::ApiKeyFactory;
use libdd_common::Endpoint;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::send_data::SendDataBuilder;
use libdd_trace_utils::trace_utils::TracerHeaderTags;
use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
use tokio::sync::Mutex;

mod common;

use common::fake_intake::FakeIntake;

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
        start: 1_700_000_000_000_000_000,
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
