// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Payload-level DSM integration test using the in-process fake-intake.
//!
//! Covers the full Data Streams Monitoring egress path:
//!
//! `DsmProcessor::record_consume` → `drain_into_proxy` (aggregate + serialize +
//! gzip) → `ProxyFlusher::flush` → `POST /api/v0.1/pipeline_stats`.
//!
//! The test spins up a `FakeIntake`, points the `DsmProcessor` at it via the
//! `apm_dd_url` argument (mirroring how `DD_APM_DD_URL` flows through in
//! production), triggers a flush, then decodes the captured msgpack payload and
//! asserts on concrete fields. Unit tests stop at the `ProxyRequest` boundary;
//! this is the only coverage of the wire format + transport + endpoint routing
//! together.

use std::collections::HashMap;
use std::sync::Arc;

use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::Config;
use bottlecap::tags::provider::Provider;
use bottlecap::traces::data_streams::processor::DsmProcessor;
use bottlecap::traces::proxy_aggregator::Aggregator as ProxyAggregator;
use bottlecap::traces::proxy_flusher::Flusher as ProxyFlusher;
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use dogstatsd::api_key::ApiKeyFactory;
use tokio::sync::Mutex;

#[path = "common/fake_intake.rs"]
mod fake_intake;

use fake_intake::FakeIntake;

const DD_API_KEY: &str = "my_test_key";

fn test_config() -> Arc<Config> {
    Arc::new(Config {
        api_key: DD_API_KEY.to_string(),
        site: "datadoghq.com".to_string(),
        ..Config::default()
    })
}

fn tags_provider(config: &Arc<Config>) -> Arc<Provider> {
    Arc::new(Provider::new(
        Arc::clone(config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([(
            "function_arn".to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        )]),
    ))
}

#[tokio::test]
async fn dsm_pipeline_stats_roundtrip_through_fake_intake() {
    let fake_intake = FakeIntake::start().await;
    let config = test_config();
    let http_client = create_reqwest_client_builder()
        .expect("failed to create reqwest client builder")
        .no_proxy()
        .build()
        .expect("failed to create reqwest client");
    let proxy_aggregator = Arc::new(Mutex::new(ProxyAggregator::default()));

    // The DsmProcessor derives its target URL from `apm_dd_url`, appending
    // `/api/v0.1/pipeline_stats`. Pointing it at the fake intake's base URL
    // exercises the same path a custom DD_APM_DD_URL takes in production.
    let dsm_processor = DsmProcessor::new(
        "fake-intake-dsm-service".to_string(),
        "test-env".to_string(),
        "1.0".to_string(),
        "2.0".to_string(),
        vec![
            "team:serverless".to_string(),
            "region:us-east-1".to_string(),
        ],
        &fake_intake.base_url(),
        Arc::clone(&proxy_aggregator),
    );

    let edge_tags = vec![
        "direction:in".to_string(),
        "topic:my-queue".to_string(),
        "type:sqs".to_string(),
    ];
    // No inbound pathway context in the carrier: this is a root consume node.
    dsm_processor.record_consume(&edge_tags, &HashMap::new(), 128.0);
    dsm_processor.drain_into_proxy().await;

    let flusher = ProxyFlusher::new(
        Arc::new(ApiKeyFactory::new(DD_API_KEY)),
        Arc::clone(&proxy_aggregator),
        tags_provider(&config),
        Arc::clone(&config),
        http_client,
    );

    // flush() awaits the HTTP response, so the intake handler has captured the
    // payload by the time this returns; no polling needed.
    let failed = flusher.flush(None).await;
    assert!(
        failed.is_none(),
        "flush reported failed requests: {failed:?}"
    );

    let payloads = fake_intake.pipeline_stats_payloads();
    assert_eq!(
        payloads.len(),
        1,
        "expected exactly one pipeline-stats payload"
    );

    let payload = &payloads[0];
    assert_eq!(payload.service, "fake-intake-dsm-service");
    assert_eq!(payload.env, "test-env");
    assert_eq!(payload.tracer_version, "1.0");
    assert_eq!(payload.version, "2.0");
    assert_eq!(payload.tags, vec!["team:serverless", "region:us-east-1"]);

    assert_eq!(payload.stats.len(), 1, "expected one stats bucket");
    let points = &payload.stats[0].stats;
    assert_eq!(points.len(), 1, "expected one stats point");

    let point = &points[0];
    assert_eq!(point.edge_tags, edge_tags);
    // Root consume node: no inbound context, so parent_hash is zero.
    assert_eq!(point.parent_hash, 0);
    assert_ne!(point.hash, 0, "consume checkpoint hash must be populated");
}
