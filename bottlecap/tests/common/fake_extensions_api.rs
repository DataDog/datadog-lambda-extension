// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! In-process fake Lambda Extensions API for process-level contract tests.
//!
//! Serves `register`, the Telemetry API `subscribe` (with a status knob), and a
//! `/next` that the test drives: push an INVOKE or a SHUTDOWN and the pending
//! long-poll returns it. It also answers any other request with `202`, so the
//! same port can stand in for the Datadog intake when a test does not need a
//! `FaultIntake`.
//!
//! It also plays Lambda's Telemetry API sender: `send_telemetry` posts a batch
//! to the extension's listener (the port taken from the subscribe body).

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{any, get, post, put},
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Default Telemetry API listener port of the extension.
pub const DEFAULT_TELEMETRY_PORT: u16 = 8999;

#[derive(Clone)]
struct Shared {
    /// Events pushed by the test, served to `/next` in order.
    next_queue: Arc<Mutex<VecDeque<Value>>>,
    next_notify: Arc<Notify>,
    next_calls: Arc<AtomicUsize>,
    /// `METHOD path` of every request, in arrival order.
    requests: Arc<Mutex<Vec<String>>>,
    subscribe_status: Arc<AtomicU16>,
    /// Port in the `destination.URI` of the last subscribe body.
    telemetry_port: Arc<AtomicU16>,
}

pub struct FakeExtensionsApi {
    pub addr: SocketAddr,
    shared: Shared,
    handle: JoinHandle<()>,
    telemetry_client: reqwest::Client,
}

/// A `/next` INVOKE body with a deadline `deadline_in` from now.
pub fn invoke_event(request_id: &str, deadline_in: Duration) -> Value {
    json!({
        "eventType": "INVOKE",
        "requestId": request_id,
        "invokedFunctionArn": "arn:aws:lambda:us-east-1:123456789012:function:test",
        "deadlineMs": epoch_ms(SystemTime::now() + deadline_in),
        "tracing": {"type": "X-Amzn-Trace-Id", "value": "Root=1-0;Parent=0;Sampled=0"}
    })
}

/// A `/next` SHUTDOWN body with a deadline `deadline_in` from now.
pub fn shutdown_event(reason: &str, deadline_in: Duration) -> Value {
    json!({
        "eventType": "SHUTDOWN",
        "shutdownReason": reason,
        "deadlineMs": epoch_ms(SystemTime::now() + deadline_in),
    })
}

fn epoch_ms(t: SystemTime) -> u64 {
    u64::try_from(t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()).unwrap_or(u64::MAX)
}

impl FakeExtensionsApi {
    /// Start the fake on `127.0.0.1:0` with an empty `/next` queue.
    pub async fn start() -> Self {
        let shared = Shared {
            next_queue: Arc::new(Mutex::new(VecDeque::new())),
            next_notify: Arc::new(Notify::new()),
            next_calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            subscribe_status: Arc::new(AtomicU16::new(StatusCode::OK.as_u16())),
            telemetry_port: Arc::new(AtomicU16::new(DEFAULT_TELEMETRY_PORT)),
        };
        let router = Router::new()
            .route("/2020-01-01/extension/register", post(register))
            .route("/2020-01-01/extension/event/next", get(next))
            .route("/2022-07-01/telemetry", put(subscribe))
            .fallback(any(accept_anything))
            .with_state(shared.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake extensions api");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            // The server ends when the test drops the fake; nothing to report.
            let _ = axum::serve(listener, router).await;
        });
        Self {
            addr,
            shared,
            handle,
            // Test-only client towards a local fake; the FIPS adapter is for production.
            #[allow(clippy::disallowed_methods)]
            telemetry_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("telemetry client"),
        }
    }

    /// `host:port`, the value for `AWS_LAMBDA_RUNTIME_API`.
    pub fn runtime_api(&self) -> String {
        self.addr.to_string()
    }

    /// `http://host:port`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Status for the Telemetry API subscribe call. Lambda returns 4xx for a
    /// bad subscription. Takes effect for calls made after it.
    pub fn set_subscribe_status(&self, status: StatusCode) {
        self.shared
            .subscribe_status
            .store(status.as_u16(), Ordering::SeqCst);
    }

    // --- driving /next ------------------------------------------------------

    /// Queue an event for the next `/next` long-poll.
    pub fn push_next(&self, event: Value) {
        self.shared
            .next_queue
            .lock()
            .expect("next queue lock")
            .push_back(event);
        self.shared.next_notify.notify_one();
    }

    pub fn push_invoke(&self, request_id: &str, deadline_in: Duration) {
        self.push_next(invoke_event(request_id, deadline_in));
    }

    pub fn push_shutdown(&self, reason: &str, deadline_in: Duration) {
        self.push_next(shutdown_event(reason, deadline_in));
    }

    /// Number of `/next` calls that arrived so far.
    pub fn next_calls(&self) -> usize {
        self.shared.next_calls.load(Ordering::SeqCst)
    }

    /// Wait until at least `n` `/next` calls arrived. Returns whether it did
    /// within `budget`.
    pub async fn wait_for_next_calls(&self, n: usize, budget: Duration) -> bool {
        let start = Instant::now();
        while self.next_calls() < n {
            if start.elapsed() > budget {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    /// `METHOD path` for each request, in order.
    pub fn request_lines(&self) -> Vec<String> {
        self.shared.requests.lock().expect("requests lock").clone()
    }

    // --- Telemetry API sender ----------------------------------------------

    /// Port the extension asked Lambda to deliver telemetry to.
    pub fn telemetry_port(&self) -> u16 {
        self.shared.telemetry_port.load(Ordering::SeqCst)
    }

    pub fn telemetry_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.telemetry_port())
    }

    /// Post a raw body (a JSON array of events) to the extension's listener.
    /// Retries until it is accepted or `budget` passes, since the listener
    /// comes up a moment after the process starts.
    pub async fn send_telemetry_raw(&self, body: Vec<u8>, budget: Duration) -> Result<(), String> {
        let start = Instant::now();
        let mut last: String;
        loop {
            let result = self
                .telemetry_client
                .post(self.telemetry_url())
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await;
            match result {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => last = format!("status {}", r.status()),
                Err(e) => last = e.to_string(),
            }
            if start.elapsed() > budget {
                return Err(format!("telemetry not accepted within {budget:?}: {last}"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Post one batch of events to the extension's listener.
    pub async fn send_telemetry(&self, events: &[Value], budget: Duration) -> Result<(), String> {
        self.send_telemetry_raw(
            Value::Array(events.to_vec()).to_string().into_bytes(),
            budget,
        )
        .await
    }
}

impl Drop for FakeExtensionsApi {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn record(shared: &Shared, method: &str, path: &str) {
    shared
        .requests
        .lock()
        .expect("requests lock")
        .push(format!("{method} {path}"));
}

async fn register(State(shared): State<Shared>) -> impl IntoResponse {
    record(&shared, "POST", "/2020-01-01/extension/register");
    let mut headers = HeaderMap::new();
    headers.insert(
        "Lambda-Extension-Identifier",
        "fake-extension-id".parse().expect("header"),
    );
    (
        StatusCode::OK,
        headers,
        json!({"accountId": "123456789012", "functionName": "test", "functionVersion": "$LATEST", "handler": "index.handler"}).to_string(),
    )
}

async fn subscribe(State(shared): State<Shared>, body: Bytes) -> impl IntoResponse {
    record(&shared, "PUT", "/2022-07-01/telemetry");
    if let Ok(v) = serde_json::from_slice::<Value>(&body)
        && let Some(uri) = v["destination"]["URI"].as_str()
        && let Some(port) = uri
            .rsplit(':')
            .next()
            .and_then(|p| p.trim_end_matches('/').parse::<u16>().ok())
    {
        shared.telemetry_port.store(port, Ordering::SeqCst);
    }
    let status = StatusCode::from_u16(shared.subscribe_status.load(Ordering::SeqCst))
        .unwrap_or(StatusCode::OK);
    (status, "{}")
}

async fn next(State(shared): State<Shared>) -> impl IntoResponse {
    record(&shared, "GET", "/2020-01-01/extension/event/next");
    shared.next_calls.fetch_add(1, Ordering::SeqCst);
    loop {
        let event = shared
            .next_queue
            .lock()
            .expect("next queue lock")
            .pop_front();
        if let Some(event) = event {
            return (StatusCode::OK, event.to_string());
        }
        shared.next_notify.notified().await;
    }
}

async fn accept_anything(
    State(shared): State<Shared>,
    request: axum::extract::Request,
) -> impl IntoResponse {
    record(&shared, request.method().as_str(), request.uri().path());
    (StatusCode::ACCEPTED, "{}")
}
