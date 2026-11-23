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

//! An OTLP metrics export posted to the OTLP agent reaches the intake unchanged, with the API
//! key and the header that turns resource attributes into tags. The agent always targets
//! `https://otlp.<site>`; here the queued request is pointed at a local intake before the flush.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::{Config, DEFAULT_APM_ERROR_TPS, LambdaConfig};
use bottlecap::otlp::agent::Agent;
use bottlecap::tags::provider::Provider;
use bottlecap::traces::proxy_aggregator::{Aggregator as ProxyAggregator, ProxyRequest};
use bottlecap::traces::proxy_flusher::Flusher as ProxyFlusher;
use bottlecap::traces::stats_concentrator_service::StatsConcentratorService;
use bottlecap::traces::stats_generator::StatsGenerator;
use bottlecap::traces::trace_processor::{ServerlessTraceProcessor, new_error_sampler};
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use dogstatsd::api_key::ApiKeyFactory;
use flate2::{Compression, write::GzEncoder};
use harness::prelude::*;
use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const METRICS_EXPORT: &str = r#"{"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"otlp-forwarding-test"}},{"key":"runtime-id","value":{"stringValue":"8f0c1c9e-runtime"}}]},"scopeMetrics":[{"scope":{"name":"test"},"metrics":[{"name":"otlp.forwarding.requests","sum":{"aggregationTemporality":1,"isMonotonic":true,"dataPoints":[{"asInt":"3","timeUnixNano":"1700000000000000000"}]}}]}]}]}"#;

#[tokio::test]
async fn metrics_export_is_forwarded_unchanged_with_metric_config_header() {
    let intake = FaultIntake::start(FaultMode::Ok).await;
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = Arc::new(Config {
        api_key: TEST_API_KEY.to_string(),
        site: "datadoghq.com".to_string(),
        otlp_config_traces_enabled: false,
        otlp_config_receiver_protocols_http_endpoint: Some(format!("127.0.0.1:{port}")),
        ext: LambdaConfig {
            metrics_otel_enabled: true,
            ..Default::default()
        },
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
    let proxy_aggregator = Arc::new(Mutex::new(ProxyAggregator::default()));

    // Traces are disabled, so the trace pipeline is only there to satisfy the constructor.
    let (trace_tx, _trace_rx) = tokio::sync::mpsc::channel(1);
    let (_concentrator, concentrator_handle) = StatsConcentratorService::new(Arc::clone(&config));
    let cancel_token = CancellationToken::new();
    let agent = Agent::new(
        Arc::clone(&config),
        Arc::clone(&tags),
        Arc::new(ServerlessTraceProcessor {
            obfuscation_config: Arc::new(ObfuscationConfig::default()),
            error_sampler: new_error_sampler(DEFAULT_APM_ERROR_TPS),
            error_rare_sample_tracer_drop: false,
        }),
        trace_tx,
        Arc::new(StatsGenerator::new(concentrator_handle)),
        Arc::clone(&proxy_aggregator),
        cancel_token.clone(),
    );
    tokio::spawn(async move {
        let _ = agent.start().await;
    });

    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(METRICS_EXPORT.as_bytes()).unwrap();
    let body = gzip.finish().unwrap();

    // Test-only client towards local servers; the FIPS adapter is what production uses.
    let client = create_reqwest_client_builder()
        .expect("client builder")
        .no_proxy()
        .build()
        .expect("client");
    let deadline = Instant::now() + secs(5);
    let response = loop {
        match client
            .post(format!("http://127.0.0.1:{port}/v1/metrics"))
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(body.clone())
            .send()
            .await
        {
            Ok(response) => break response,
            Err(e) if e.is_connect() && Instant::now() < deadline => {
                tokio::time::sleep(ms(20)).await;
            }
            Err(e) => panic!("OTLP agent did not accept the export: {e}"),
        }
    };
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );

    let mut queued = proxy_aggregator.lock().await.get_batch();
    assert_eq!(queued.len(), 1);
    let request = queued.pop().unwrap();
    assert_eq!(request.target_url, "https://otlp.datadoghq.com/v1/metrics");
    proxy_aggregator.lock().await.add(ProxyRequest {
        target_url: format!("{}/v1/metrics", intake.url()),
        ..request
    });

    let flusher = ProxyFlusher::new(
        Arc::new(ApiKeyFactory::new(TEST_API_KEY)),
        Arc::clone(&proxy_aggregator),
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

    let sent = intake.requests_to("/v1/metrics");
    assert_eq!(sent.len(), 1, "{}", intake.summary());
    assert_eq!(sent[0].api_key(), Some(TEST_API_KEY));
    assert_eq!(
        sent[0].header("dd-otel-metric-config"),
        Some(r#"{"resource_attributes_as_tags": true}"#)
    );
    assert_eq!(sent[0].header("content-type"), Some("application/json"));
    assert_eq!(sent[0].header("content-encoding"), Some("gzip"));
    assert_eq!(sent[0].body, body);
    assert_eq!(sent[0].body_text().unwrap(), METRICS_EXPORT);

    cancel_token.cancel();
}
