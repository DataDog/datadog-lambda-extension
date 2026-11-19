// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// Integration tests are exempt from the production lint policy in Cargo.toml.
#![allow(
    unreachable_pub,
    clippy::pedantic,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used
)]

//! The proxy flusher sends a queued flag event batch with the API key and the
//! tracer's identity headers. The trace agent builds the intake URL; here the
//! batch targets a local intake directly.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use std::collections::HashMap;
use std::sync::Arc;

use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::Config;
use bottlecap::tags::provider::Provider;
use bottlecap::traces::proxy_aggregator::{Aggregator as ProxyAggregator, ProxyRequest};
use bottlecap::traces::proxy_flusher::Flusher as ProxyFlusher;
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use dogstatsd::api_key::ApiKeyFactory;
use harness::prelude::*;
use tokio::sync::Mutex;

#[tokio::test]
async fn flag_event_batch_is_flushed_with_api_key_and_origin_headers() {
    let intake = FaultIntake::start(FaultMode::Ok).await;
    let config = Arc::new(Config {
        api_key: TEST_API_KEY.to_string(),
        site: "datadoghq.com".to_string(),
        ..Config::default()
    });
    let tags = Arc::new(Provider::new(
        Arc::clone(&config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([(
            "function_arn".to_string(),
            "arn:aws:lambda:us-east-1:123456789012:function:test".to_string(),
        )]),
    ));
    let aggregator = Arc::new(Mutex::new(ProxyAggregator::default()));
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("dd-evp-origin", "dd-trace-js".parse().unwrap());
    headers.insert("dd-evp-origin-version", "5.80.0".parse().unwrap());
    aggregator.lock().await.add(ProxyRequest {
        headers,
        body: r#"{"exposures":[{"flag":{"key":"harness-exposed-flag"}}]}"#.into(),
        target_url: format!("{}/api/v2/exposures", intake.url()),
    });
    // Test-only client towards a local intake; the FIPS adapter is what production uses.
    let client = create_reqwest_client_builder()
        .expect("client builder")
        .no_proxy()
        .build()
        .expect("client");
    let flusher = ProxyFlusher::new(
        Arc::new(ApiKeyFactory::new(TEST_API_KEY)),
        Arc::clone(&aggregator),
        tags,
        Arc::clone(&config),
        client,
    );

    let failed = flusher.flush(None).await;
    assert!(
        failed.is_none(),
        "flush failed: {failed:?}. {}",
        intake.summary()
    );

    let sent = intake.requests_to("/api/v2/exposures");
    assert_eq!(sent.len(), 1, "{}", intake.summary());
    assert_eq!(sent[0].api_key(), Some(TEST_API_KEY));
    assert_eq!(sent[0].header("dd-evp-origin"), Some("dd-trace-js"));
    assert_eq!(sent[0].header("dd-evp-origin-version"), Some("5.80.0"));
    assert!(
        sent[0]
            .body_text()
            .unwrap()
            .contains("harness-exposed-flag")
    );
}
