// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! In-process fake Lambda Extensions API for process-level contract tests.
//!
//! Serves `register`, the Telemetry API `subscribe` (with a status knob), and a
//! `/next` that the test drives: push an INVOKE or a SHUTDOWN and the pending
//! long-poll returns it. Like Lambda, `/next` serves only the event types the
//! extension registered for, `/next` and `subscribe` get a `403` without the
//! identifier issued at register, register gets a `403` without the
//! extension's name, and a malformed register body gets a `400`.
//! It also answers any other request with `202`, so the same port can stand in
//! for the Datadog intake when a test does not need a `FaultIntake`.
//!
//! It also plays Lambda's Telemetry API sender: `send_telemetry` posts a batch
//! to the extension's listener (the port taken from the subscribe body). Like
//! Lambda, it delivers nothing until a subscribe call was accepted, and then
//! only the event types that call asked for.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
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

const EXTENSION_NAME_HEADER: &str = "Lambda-Extension-Name";
/// The extension's file name under `/opt/extensions`.
const EXTENSION_NAME: &str = "datadog-agent";
const EXTENSION_ID_HEADER: &str = "Lambda-Extension-Identifier";
/// The identifier the fake issues at register.
const EXTENSION_ID: &str = "fake-extension-id";

#[derive(Clone)]
struct Shared {
    /// Events pushed by the test, served to `/next` in order.
    next_queue: Arc<Mutex<VecDeque<Value>>>,
    next_notify: Arc<Notify>,
    /// Arrival time of every accepted `/next` call, in order.
    next_arrivals: Arc<Mutex<Vec<Instant>>>,
    /// Event types from the last accepted register body.
    registered_events: Arc<Mutex<Vec<String>>>,
    /// `METHOD path` of every request, in arrival order.
    requests: Arc<Mutex<Vec<String>>>,
    subscribe_status: Arc<AtomicU16>,
    /// Port in the `destination.URI` of the last accepted subscribe body, 0
    /// until one.
    telemetry_port: Arc<AtomicU16>,
    /// `types` of the last accepted subscribe body.
    telemetry_types: Arc<Mutex<Vec<String>>>,
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
            next_arrivals: Arc::new(Mutex::new(Vec::new())),
            registered_events: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            subscribe_status: Arc::new(AtomicU16::new(StatusCode::OK.as_u16())),
            telemetry_port: Arc::new(AtomicU16::new(0)),
            telemetry_types: Arc::new(Mutex::new(Vec::new())),
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
        self.shared
            .next_arrivals
            .lock()
            .expect("next arrivals lock")
            .len()
    }

    /// Wait for `/next` call number `n`. Returns whether it arrived by
    /// `deadline`, so a call that came late counts as missed even when the
    /// wait starts after it.
    pub async fn wait_for_next_calls(&self, n: usize, deadline: Instant) -> bool {
        loop {
            let now = Instant::now();
            let arrived = self
                .shared
                .next_arrivals
                .lock()
                .expect("next arrivals lock")
                .get(n - 1)
                .copied();
            if let Some(at) = arrived {
                return at <= deadline;
            }
            if now > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// `METHOD path` for each request, in order.
    pub fn request_lines(&self) -> Vec<String> {
        self.shared.requests.lock().expect("requests lock").clone()
    }

    // --- Telemetry API sender ----------------------------------------------

    /// Port the extension asked Lambda to deliver telemetry to, `None` until
    /// a subscribe call was accepted.
    pub fn telemetry_port(&self) -> Option<u16> {
        match self.shared.telemetry_port.load(Ordering::SeqCst) {
            0 => None,
            port => Some(port),
        }
    }

    pub fn telemetry_url(&self) -> Option<String> {
        self.telemetry_port()
            .map(|port| format!("http://127.0.0.1:{port}/"))
    }

    /// Post a raw body (a JSON array of events) to the extension's listener.
    /// Retries until it is accepted or `budget` passes, since the listener
    /// comes up a moment after the process starts. Fails without an accepted
    /// subscription, as Lambda would deliver nothing.
    pub async fn send_telemetry_raw(&self, body: Vec<u8>, budget: Duration) -> Result<(), String> {
        let start = Instant::now();
        let mut last: String;
        loop {
            let result = match self.telemetry_url() {
                Some(url) => self
                    .telemetry_client
                    .post(url)
                    .header("content-type", "application/json")
                    .body(body.clone())
                    .send()
                    .await
                    .map_err(|e| e.to_string()),
                None => Err("no accepted Telemetry API subscription".to_string()),
            };
            match result {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => last = format!("status {}", r.status()),
                Err(e) => last = e,
            }
            if start.elapsed() > budget {
                return Err(format!("telemetry not accepted within {budget:?}: {last}"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Post one batch of events to the extension's listener. Like Lambda, it
    /// withholds events whose type (`platform`, `function`, `extension`) the
    /// subscription did not ask for, and posts nothing when none is left.
    pub async fn send_telemetry(&self, events: &[Value], budget: Duration) -> Result<(), String> {
        if self.telemetry_port().is_none() {
            return Err("no accepted Telemetry API subscription".to_string());
        }
        let types = self
            .shared
            .telemetry_types
            .lock()
            .expect("telemetry types lock")
            .clone();
        let (subscribed, withheld): (Vec<Value>, Vec<Value>) =
            events.iter().cloned().partition(|e| {
                let kind = e["type"].as_str().unwrap_or_default();
                types
                    .iter()
                    .any(|t| kind.split('.').next() == Some(t.as_str()))
            });
        for e in &withheld {
            eprintln!(
                "fake extensions api: withheld an unsubscribed {} event",
                e["type"]
            );
        }
        if subscribed.is_empty() {
            return Ok(());
        }
        self.send_telemetry_raw(Value::Array(subscribed).to_string().into_bytes(), budget)
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

async fn register(
    State(shared): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    record(&shared, "POST", "/2020-01-01/extension/register");
    // Lambda answers `403` without a name. Any name but the file under
    // `/opt/extensions` registers an internal extension, which cannot take SHUTDOWN.
    if headers
        .get(EXTENSION_NAME_HEADER)
        .is_none_or(|v| v != EXTENSION_NAME)
    {
        return Err(StatusCode::FORBIDDEN);
    }
    // Lambda accepts only these event types and rejects any other body.
    let events = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| {
            v["events"]
                .as_array()?
                .iter()
                .map(|e| {
                    e.as_str()
                        .filter(|e| matches!(*e, "INVOKE" | "SHUTDOWN"))
                        .map(str::to_string)
                })
                .collect::<Option<Vec<_>>>()
        })
        .ok_or(StatusCode::BAD_REQUEST)?;
    *shared
        .registered_events
        .lock()
        .expect("registered events lock") = events;
    let mut headers = HeaderMap::new();
    headers.insert(EXTENSION_ID_HEADER, EXTENSION_ID.parse().expect("header"));
    Ok((
        StatusCode::OK,
        headers,
        json!({"accountId": "123456789012", "functionName": "test", "functionVersion": "$LATEST", "handler": "index.handler"}).to_string(),
    ))
}

/// Lambda answers `403` to a `/next` or `subscribe` call without the
/// identifier issued at register.
fn check_extension_id(headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(EXTENSION_ID_HEADER)
        .is_some_and(|v| v == EXTENSION_ID)
    {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn subscribe(
    State(shared): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    record(&shared, "PUT", "/2022-07-01/telemetry");
    check_extension_id(&headers)?;
    let status = StatusCode::from_u16(shared.subscribe_status.load(Ordering::SeqCst))
        .unwrap_or(StatusCode::OK);
    if status.is_success()
        && let Ok(v) = serde_json::from_slice::<Value>(&body)
        && let Some(uri) = v["destination"]["URI"].as_str()
        && let Some(port) = uri
            .rsplit(':')
            .next()
            .and_then(|p| p.trim_end_matches('/').parse::<u16>().ok())
    {
        *shared.telemetry_types.lock().expect("telemetry types lock") = v["types"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect();
        shared.telemetry_port.store(port, Ordering::SeqCst);
    }
    Ok((status, "{}"))
}

async fn next(
    State(shared): State<Shared>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    record(&shared, "GET", "/2020-01-01/extension/event/next");
    check_extension_id(&headers)?;
    shared
        .next_arrivals
        .lock()
        .expect("next arrivals lock")
        .push(Instant::now());
    loop {
        let event = shared
            .next_queue
            .lock()
            .expect("next queue lock")
            .pop_front();
        let Some(event) = event else {
            shared.next_notify.notified().await;
            continue;
        };
        let event_type = event["eventType"].as_str().unwrap_or_default();
        let registered = shared
            .registered_events
            .lock()
            .expect("registered events lock")
            .iter()
            .any(|e| e == event_type);
        if registered {
            return Ok((StatusCode::OK, event.to_string()));
        }
        // Lambda never sends an event type the extension did not register for.
        eprintln!("fake extensions api: dropped an unregistered {event_type} event");
    }
}

async fn accept_anything(
    State(shared): State<Shared>,
    request: axum::extract::Request,
) -> impl IntoResponse {
    record(&shared, request.method().as_str(), request.uri().path());
    (StatusCode::ACCEPTED, "{}")
}
