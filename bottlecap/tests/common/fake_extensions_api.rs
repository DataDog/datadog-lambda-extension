// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! In-process fake Lambda Extensions API for process-level contract tests.
//!
//! Serves `register`, the Telemetry API `subscribe` (with a status knob), and a
//! `/next` that the test drives: push an INVOKE or a SHUTDOWN and the pending
//! long-poll returns it. Like Lambda, `/next` serves only the event types the
//! extension registered for, a `/next` while another is pending gets a `403`,
//! `/next` and `subscribe` get a `403` without the identifier issued at
//! register, register gets a `403` without the extension's name or with an
//! invalid body, an invalid subscribe body gets a `400`, and register returns
//! `accountId` only when the extension asks for it. In Lambda Managed Instances
//! mode ([`set_managed_instance`](FakeExtensionsApi::set_managed_instance)),
//! register gets a `403` for `INVOKE` and subscribe a `400` for any schema
//! version but `2025-01-29`.
//! It also answers any other request with `202`, so the same port can stand in
//! for the Datadog intake when a test does not need a `FaultIntake`.
//!
//! It also plays Lambda's Telemetry API sender: `send_telemetry` posts a batch
//! to the extension's listener (the port and path of the subscribe URI). Like
//! Lambda, it delivers nothing until a subscribe call was accepted, and then
//! only the event types that call asked for.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
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
const ACCEPT_FEATURE_HEADER: &str = "Lambda-Extension-Accept-Feature";
/// The extension's file name under `/opt/extensions`.
const EXTENSION_NAME: &str = "datadog-agent";
const EXTENSION_ID_HEADER: &str = "Lambda-Extension-Identifier";
/// The identifier the fake issues at register, a UUID like Lambda's.
const EXTENSION_ID: &str = "3f8e4b6a-1c2d-4e5f-8a9b-0c1d2e3f4a5b";

#[derive(Clone)]
struct Shared {
    /// Events pushed by the test, served to `/next` in order.
    next_queue: Arc<Mutex<VecDeque<Value>>>,
    next_notify: Arc<Notify>,
    /// Arrival time of every accepted `/next` call, in order.
    next_arrivals: Arc<Mutex<Vec<Instant>>>,
    /// An accepted `/next` call waits for an event.
    next_pending: Arc<AtomicBool>,
    /// Lambda Managed Instances rules for register and subscribe.
    managed_instance: Arc<AtomicBool>,
    /// Event types from the last accepted register body.
    registered_events: Arc<Mutex<Vec<String>>>,
    /// `METHOD path` of every request, in arrival order.
    requests: Arc<Mutex<Vec<String>>>,
    subscribe_status: Arc<AtomicU16>,
    /// `destination.URI` of the last accepted subscribe body with the host set
    /// to `127.0.0.1`, `None` until one.
    telemetry_url: Arc<Mutex<Option<reqwest::Url>>>,
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
            next_pending: Arc::new(AtomicBool::new(false)),
            managed_instance: Arc::new(AtomicBool::new(false)),
            registered_events: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            subscribe_status: Arc::new(AtomicU16::new(StatusCode::OK.as_u16())),
            telemetry_url: Arc::new(Mutex::new(None)),
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

    /// Apply the Lambda Managed Instances rules to calls made after it.
    pub fn set_managed_instance(&self) {
        self.shared.managed_instance.store(true, Ordering::SeqCst);
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
        self.telemetry_url_parsed()?.port()
    }

    /// Where Lambda would post telemetry: the subscribed port and path.
    pub fn telemetry_url(&self) -> Option<String> {
        self.telemetry_url_parsed().map(String::from)
    }

    fn telemetry_url_parsed(&self) -> Option<reqwest::Url> {
        self.shared
            .telemetry_url
            .lock()
            .expect("telemetry url lock")
            .clone()
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
    let managed_instance = shared.managed_instance.load(Ordering::SeqCst);
    let events = register_events(&body, managed_instance).ok_or(StatusCode::FORBIDDEN)?;
    *shared
        .registered_events
        .lock()
        .expect("registered events lock") = events;
    let mut response =
        json!({"functionName": "test", "functionVersion": "$LATEST", "handler": "index.handler"});
    // Lambda adds `accountId` only when the extension asks for that feature.
    if headers
        .get(ACCEPT_FEATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|f| f.trim() == "accountId"))
    {
        response["accountId"] = json!("123456789012");
    }
    let mut headers = HeaderMap::new();
    headers.insert(EXTENSION_ID_HEADER, EXTENSION_ID.parse().expect("header"));
    Ok((StatusCode::OK, headers, Json(response)))
}

/// Event types of a register body, `None` when Lambda answers `403`. Lambda
/// accepts a body without `events`, and only `INVOKE` and `SHUTDOWN` in it
/// (only `SHUTDOWN` on Lambda Managed Instances).
fn register_events(body: &[u8], managed_instance: bool) -> Option<Vec<String>> {
    let body = serde_json::from_slice::<Value>(body).ok()?;
    match body.as_object()?.get("events") {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(events) => events
            .as_array()?
            .iter()
            .map(|e| {
                e.as_str()
                    .filter(|e| *e == "SHUTDOWN" || (*e == "INVOKE" && !managed_instance))
                    .map(str::to_string)
            })
            .collect(),
    }
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
    // The Subscribe API takes a JSON body with `Content-Type: application/json`.
    if !headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let managed_instance = shared.managed_instance.load(Ordering::SeqCst);
    let (url, types) = subscription(&body, managed_instance).ok_or(StatusCode::BAD_REQUEST)?;
    let status = StatusCode::from_u16(shared.subscribe_status.load(Ordering::SeqCst))
        .unwrap_or(StatusCode::OK);
    if status.is_success() {
        *shared.telemetry_types.lock().expect("telemetry types lock") = types;
        *shared.telemetry_url.lock().expect("telemetry url lock") = Some(url);
    }
    Ok((status, "{}"))
}

/// Delivery URL and `types` of a subscribe body, `None` when Lambda answers
/// `400`: the Telemetry API reference rules, and an `http://sandbox` URI with a
/// port (Lambda does not deliver over HTTPS). Lambda Managed Instances take
/// only schema `2025-01-29`. The fake sender speaks only HTTP, so it also
/// rejects a `TCP` destination.
fn subscription(body: &[u8], managed_instance: bool) -> Option<(reqwest::Url, Vec<String>)> {
    let v = serde_json::from_slice::<Value>(body).ok()?;
    let types = v["types"]
        .as_array()
        .filter(|types| !types.is_empty())?
        .iter()
        .map(|t| {
            t.as_str()
                .filter(|t| matches!(*t, "platform" | "function" | "extension"))
                .map(str::to_string)
        })
        .collect::<Option<Vec<_>>>()?;
    let buffering_ok = v.get("buffering").is_none_or(|b| {
        let in_range = |key: &str, range: RangeInclusive<u64>| {
            b.get(key)
                .is_none_or(|n| n.as_u64().is_some_and(|n| range.contains(&n)))
        };
        b.is_object()
            && in_range("maxItems", 1_000..=10_000)
            && in_range("maxBytes", 262_144..=1_048_576)
            && in_range("timeoutMs", 25..=30_000)
    });
    let schema_ok = match v["schemaVersion"].as_str() {
        Some("2025-01-29") => true,
        Some("2022-07-01" | "2022-12-13") => !managed_instance,
        _ => false,
    };
    let mut uri = reqwest::Url::parse(v["destination"]["URI"].as_str()?).ok()?;
    let valid = schema_ok
        && v["destination"]["protocol"] == "HTTP"
        && uri.scheme() == "http"
        && matches!(uri.host_str(), Some("sandbox" | "sandbox.localdomain"))
        // Lambda reserves port 9001 for the Runtime API.
        && uri.port().is_some_and(|p| p != 0 && p != 9001)
        && buffering_ok;
    if !valid {
        return None;
    }
    // Lambda posts to the URI's port and path inside the sandbox.
    uri.set_host(Some("127.0.0.1")).ok()?;
    Some((uri, types))
}

async fn next(
    State(shared): State<Shared>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, StatusCode> {
    record(&shared, "GET", "/2020-01-01/extension/event/next");
    check_extension_id(&headers)?;
    // Lambda answers `403` to a `/next` while the extension's last one waits.
    // A poll the client drops stays pending, as in Lambda.
    if shared.next_pending.swap(true, Ordering::SeqCst) {
        return Err(StatusCode::FORBIDDEN);
    }
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
            shared.next_pending.store(false, Ordering::SeqCst);
            return Ok((StatusCode::OK, Json(event)));
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
