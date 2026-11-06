// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reproduce ambiguous-delivery duplication through the real test-mode binary.
//!
//! A test-local proxy forwards requests to the shared mock intake. In the
//! ambiguous-accept arm it waits for the intake's 202, then closes the connection
//! without sending a response. The stats flusher retries the same bytes, so the
//! mock intake captures two accepted payloads for one logical send.
//!
//! Controls cover normal delivery and a 500 rejection followed by a retry.
//! This proves the local retry mechanism, not real Datadog intake dedup behavior.

use std::io;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use datadog_mock_intake::{MockIntake, MockIntakeOptions};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use libdd_trace_protobuf::pb;
use rmp_serde::to_vec_named;
use tokio::task::{JoinHandle, JoinSet};

const FLUSH_WAIT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const STATS_PATH: &str = "/api/v0.2/stats";

/// Kills and reaps the subprocess even when an assertion fails.
struct TestModeProcess(Child);

impl Drop for TestModeProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct ResponseDropProxy {
    base_url: String,
    dropped: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Drop for ResponseDropProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ResponseDropProxy {
    async fn start(upstream: String, drop_first_stats_response: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy must bind");
        let base_url = format!("http://{}", listener.local_addr().expect("proxy address"));
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_flag = Arc::clone(&dropped);
        let client = create_reqwest_client_builder()
            .expect("proxy client builder must initialize")
            .no_proxy()
            .timeout(FLUSH_WAIT)
            .build()
            .expect("proxy client must build");
        let task = tokio::spawn(async move {
            // Dropping this set aborts connections when the proxy is stopped.
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.expect("proxy must accept");
                        let client = client.clone();
                        let upstream = upstream.clone();
                        let dropped = Arc::clone(&dropped_flag);
                        connections.spawn(async move {
                            let service = service_fn(move |request| {
                                forward_request(
                                    request,
                                    client.clone(),
                                    upstream.clone(),
                                    drop_first_stats_response,
                                    Arc::clone(&dropped),
                                )
                            });
                            // Returning a service error deliberately closes the connection
                            // without response headers for the ambiguous-accept arm.
                            http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service)
                                .await
                        });
                    }
                    result = connections.join_next(), if !connections.is_empty() => {
                        let result = result.expect("connection task exists").expect("proxy connection panicked");
                        if let Err(error) = result {
                            assert!(error.is_user(), "unexpected proxy connection error: {error}");
                        }
                    }
                }
            }
        });
        Self {
            base_url,
            dropped,
            task,
        }
    }
}

async fn forward_request(
    request: Request<Incoming>,
    client: reqwest::Client,
    upstream: String,
    drop_first_stats_response: bool,
    dropped: Arc<AtomicBool>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let (mut parts, body) = request.into_parts();
    let is_stats = parts.uri.path() == STATS_PATH;
    parts.headers.remove(hyper::header::HOST);
    let body = body.collect().await.map_err(io::Error::other)?.to_bytes();
    let response = client
        .request(parts.method, format!("{upstream}{}", parts.uri))
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .map_err(io::Error::other)?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(io::Error::other)?;
    if is_stats
        && status == StatusCode::ACCEPTED
        && drop_first_stats_response
        && !dropped.swap(true, Ordering::SeqCst)
    {
        return Err(io::Error::other(
            "injected response loss after intake acceptance",
        ));
    }
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    Ok(response)
}

/// Reserve a free port. The brief bind race is acceptable for this harness.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("test: failed to reserve a port")
        .local_addr()
        .expect("test: failed to read reserved port")
        .port()
}

fn trace_payload_bytes() -> Vec<u8> {
    let span = pb::Span {
        service: "smoke-svc".to_string(),
        name: "smoke.request".to_string(),
        resource: "GET /smoke".to_string(),
        r#type: "web".to_string(),
        trace_id: 1_234_567_890_123,
        span_id: 987_654_321,
        start: 1_700_000_000_000_000_000,
        duration: 15_000_000,
        error: 0,
        ..pb::Span::default()
    };
    to_vec_named(&vec![vec![span]]).expect("test: msgpack encode failed")
}

async fn start_harness(
    options: MockIntakeOptions,
    drop_first_stats_response: bool,
) -> (MockIntake, TestModeProcess, ResponseDropProxy) {
    let intake = MockIntake::start_with_options(options)
        .await
        .expect("test: mock intake failed to start");
    let proxy = ResponseDropProxy::start(intake.base_url(), drop_first_stats_response).await;
    let receiver_port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_bottlecap-test-mode"))
        .env_clear()
        .env("DD_APM_DD_URL", &proxy.base_url)
        .env("DD_APM_RECEIVER_PORT", receiver_port.to_string())
        .env("DD_LAMBDA_EXTENSION_COMPUTE_STATS", "true")
        .env("DD_SERVICE", "double-count-smoke")
        .env("DD_ENV", "local")
        .env("DD_VERSION", "smoke")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("test: failed to spawn bottlecap-test-mode");
    let process = TestModeProcess(child);
    let receiver = format!("http://127.0.0.1:{receiver_port}");
    let client = create_reqwest_client_builder()
        .expect("harness client builder must initialize")
        .no_proxy()
        .timeout(FLUSH_WAIT)
        .build()
        .expect("harness client must build");
    let deadline = tokio::time::Instant::now() + FLUSH_WAIT;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "test-mode did not answer /info in time"
        );
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

    let response = client
        .post(format!("{receiver}/v0.4/traces"))
        .header("Content-Type", "application/msgpack")
        .header("X-Datadog-Trace-Count", "1")
        .body(trace_payload_bytes())
        .send()
        .await
        .expect("test: trace ingest request failed");
    assert!(
        response.status().is_success(),
        "trace ingest failed: {}",
        response.status()
    );

    // The ingest barrier and flush lock make this flush deterministic. Retried
    // deliveries must still succeed, not merely return a payload-loss status.
    let flush = client
        .post(format!("{receiver}/flush"))
        .send()
        .await
        .expect("test: /flush request failed");
    assert_eq!(
        flush.status(),
        StatusCode::NO_CONTENT,
        "flush must deliver all payloads"
    );
    assert!(!proxy.task.is_finished(), "proxy unexpectedly stopped");
    (intake, process, proxy)
}

#[tokio::test]
async fn baseline_delivers_exactly_one_stats_payload() {
    let (intake, _process, proxy) = start_harness(MockIntakeOptions::default(), false).await;
    assert_eq!(
        intake.stats_payloads().len(),
        1,
        "exactly one accepted payload"
    );
    assert_eq!(intake.requests_for_path(STATS_PATH).len(), 1, "no retries");
    assert!(!proxy.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn safe_rejection_retries_but_does_not_double_count() {
    let (intake, _process, proxy) = start_harness(
        MockIntakeOptions {
            fail_stats_first_n: 1,
            ..MockIntakeOptions::default()
        },
        false,
    )
    .await;
    assert_eq!(
        intake.stats_payloads().len(),
        1,
        "only the retry is accepted"
    );
    let attempts = intake.requests_for_path(STATS_PATH);
    assert_eq!(attempts.len(), 2, "one rejected attempt and one retry");
    assert_eq!(
        attempts[0].body, attempts[1].body,
        "retry must preserve wire bytes"
    );
    assert!(!proxy.dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn ambiguous_accept_is_retried_and_double_counts() {
    let (intake, _process, proxy) = start_harness(MockIntakeOptions::default(), true).await;
    assert!(
        proxy.dropped.load(Ordering::SeqCst),
        "proxy must have discarded an intake 202"
    );
    let captured = intake.stats_payloads();
    assert_eq!(
        captured.len(),
        2,
        "both the ambiguous accept and retry are accepted"
    );
    assert_eq!(captured[0], captured[1], "decoded stats must be identical");
    let attempts = intake.requests_for_path(STATS_PATH);
    assert_eq!(attempts.len(), 2, "one ambiguous accept and one retry");
    assert_eq!(
        attempts[0].body, attempts[1].body,
        "retry must preserve wire bytes"
    );
    let total_hits: u64 = captured
        .iter()
        .flat_map(|payload| &payload.stats)
        .flat_map(|client| &client.stats)
        .flat_map(|bucket| &bucket.stats)
        .map(|group| group.hits)
        .sum();
    assert_eq!(total_hits, 2, "one source span produces two accepted hits");
}
