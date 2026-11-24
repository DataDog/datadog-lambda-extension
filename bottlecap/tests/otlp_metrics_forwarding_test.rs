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

//! OTLP metrics forwarding contract: with `DD_METRICS_OTEL_ENABLED`, as a tracer sets it, the
//! real `bottlecap` binary accepts an OTLP export on the tracers' default port and forwards it
//! unchanged to the OTLP intake, with the API key and the metric config header.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use std::io::Write;

use flate2::{Compression, write::GzEncoder};
use harness::prelude::*;

const OTLP_HTTP_PORT: u16 = 4318;
const METRICS_PATH: &str = "/v1/metrics";
const METRICS_EXPORT: &str = r#"{"resourceMetrics":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"otlp-forwarding-test"}},{"key":"runtime-id","value":{"stringValue":"8f0c1c9e-runtime"}}]},"scopeMetrics":[{"scope":{"name":"test"},"metrics":[{"name":"otlp.forwarding.requests","sum":{"aggregationTemporality":1,"isMonotonic":true,"dataPoints":[{"asInt":"3","timeUnixNano":"1700000000000000000"}]}}]}]}]}"#;

/// POST a gzipped JSON export to the extension's OTLP listener, retrying until it accepts the
/// connection. Returns the HTTP status.
async fn post_metrics(body: Vec<u8>, budget: Duration) -> Result<u16, String> {
    // Test-only client towards the local OTLP listener; the FIPS adapter is for production.
    #[allow(clippy::disallowed_methods)]
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    let start = std::time::Instant::now();
    let mut last: String;
    loop {
        let result = client
            .post(format!("http://127.0.0.1:{OTLP_HTTP_PORT}{METRICS_PATH}"))
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(body.clone())
            .send()
            .await;
        match result {
            Ok(r) => return Ok(r.status().as_u16()),
            Err(e) => last = e.to_string(),
        }
        if start.elapsed() > budget {
            return Err(format!(
                "OTLP listener not reachable within {budget:?}: {last}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn metrics_export_is_forwarded_unchanged_with_metric_config_header() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().env("DD_METRICS_OTEL_ENABLED", "true").spawn();
    let deadline = s
        .invoke("req-1", secs(10), 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));

    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(METRICS_EXPORT.as_bytes()).unwrap();
    let body = gzip.finish().unwrap();
    let status = post_metrics(body.clone(), STARTUP_BUDGET).await.unwrap();

    deliver_start(&s.api, "req-1", true).await.unwrap();
    deliver_runtime_done(&s.api, "req-1", true).await.unwrap();
    let released = s.api.wait_for_next_calls(2, deadline).await;
    let forwarded = s
        .intake
        .wait_for(
            1,
            |r| {
                r.path == METRICS_PATH
                    && r.api_key() == Some(TEST_API_KEY)
                    && r.body_text().is_ok_and(|text| text == METRICS_EXPORT)
            },
            deadline,
        )
        .await;

    s.shutdown_cleanly(&mut bc).await;
    assert_eq!(status, 200, "the OTLP listener rejected the export");
    assert!(
        released,
        "no /next after the invocation. {}",
        bc.log_tail(40)
    );
    assert!(
        forwarded,
        "the export never reached the OTLP intake with the API key by its invoke deadline. Intake:\n{}\n{}",
        s.intake.summary(),
        bc.log_tail(40)
    );
    let sent = s.intake.requests_to(METRICS_PATH);
    let request = sent.first().unwrap();
    assert_eq!(
        request.header("dd-otel-metric-config"),
        Some(
            r#"{"resource_attributes_as_tags": false, "instrumentation_scope_metadata_as_tags": true}"#
        )
    );
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("content-encoding"), Some("gzip"));
    assert_eq!(
        request.body, body,
        "the export was not forwarded byte for byte"
    );
}
