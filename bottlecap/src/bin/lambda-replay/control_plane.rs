//! A fake Lambda control plane: the Runtime API for the function, the
//! Extensions and Telemetry APIs for bottlecap, and a Datadog intake that
//! keeps what bottlecap flushes.

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use bottlecap::replay::format::{Headers, ResultKind};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};

const EXTENSION_ID: &str = "lambda-replay-extension-id";

pub struct RuntimeEvent {
    pub request_id: String,
    pub headers: Headers,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InvocationResult {
    pub kind: ResultKind,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct IntakeRequest {
    pub path: String,
    pub body: Vec<u8>,
}

pub struct Shared {
    runtime_events: tokio::sync::Mutex<mpsc::UnboundedReceiver<RuntimeEvent>>,
    extension_events: tokio::sync::Mutex<mpsc::UnboundedReceiver<Value>>,
    pub results: Mutex<HashMap<String, oneshot::Sender<InvocationResult>>>,
    pub runtime_next_calls: watch::Sender<usize>,
    pub extension_next_calls: watch::Sender<usize>,
    pub telemetry_uri: Mutex<Option<String>>,
    pub init_error: Mutex<Option<String>>,
    /// The invocation in flight, shared with the replay MITM for attribution.
    pub current: Arc<Mutex<Option<String>>>,
    pub intake: Mutex<Vec<IntakeRequest>>,
    function_name: String,
    handler: String,
}

pub struct ControlPlane {
    pub addr: SocketAddr,
    pub shared: Arc<Shared>,
    pub runtime_tx: mpsc::UnboundedSender<RuntimeEvent>,
    pub extension_tx: mpsc::UnboundedSender<Value>,
}

impl ControlPlane {
    pub async fn start(
        function_name: String,
        handler: String,
        current: Arc<Mutex<Option<String>>>,
    ) -> std::io::Result<Self> {
        let (runtime_tx, runtime_rx) = mpsc::unbounded_channel();
        let (extension_tx, extension_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            runtime_events: tokio::sync::Mutex::new(runtime_rx),
            extension_events: tokio::sync::Mutex::new(extension_rx),
            results: Mutex::default(),
            runtime_next_calls: watch::channel(0).0,
            extension_next_calls: watch::channel(0).0,
            telemetry_uri: Mutex::default(),
            init_error: Mutex::default(),
            current,
            intake: Mutex::default(),
            function_name,
            handler,
        });
        let router = Router::new()
            .route("/2018-06-01/runtime/invocation/next", get(runtime_next))
            .route(
                "/2018-06-01/runtime/invocation/{id}/response",
                post(|s, p, h, b| runtime_result(s, p, h, b, ResultKind::Response)),
            )
            .route(
                "/2018-06-01/runtime/invocation/{id}/error",
                post(|s, p, h, b| runtime_result(s, p, h, b, ResultKind::Error)),
            )
            .route("/2018-06-01/runtime/init/error", post(init_error))
            .route("/2020-01-01/extension/register", post(register))
            .route("/2020-01-01/extension/event/next", get(extension_next))
            .route("/2022-07-01/telemetry", put(subscribe))
            // Anything else is bottlecap flushing to its Datadog intake.
            .fallback(intake)
            .with_state(Arc::clone(&shared));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            addr,
            shared,
            runtime_tx,
            extension_tx,
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn runtime_next(State(s): State<Arc<Shared>>) -> Response {
    s.runtime_next_calls.send_modify(|n| *n += 1);
    let mut rx = s.runtime_events.lock().await;
    let Some(event) = rx.recv().await else {
        // No more invocations: hold the runtime until it is killed, as Lambda would.
        drop(rx);
        std::future::pending::<()>().await;
        unreachable!();
    };
    *lock(&s.current) = Some(event.request_id.clone());
    let mut headers = HeaderMap::new();
    for (name, values) in &event.headers {
        if let (Ok(n), Some(v)) = (
            axum::http::HeaderName::from_bytes(name.as_bytes()),
            values.first().and_then(|v| HeaderValue::from_str(v).ok()),
        ) {
            headers.insert(n, v);
        }
    }
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    (StatusCode::OK, headers, event.body).into_response()
}

async fn runtime_result(
    State(s): State<Arc<Shared>>,
    Path(id): Path<String>,
    _headers: HeaderMap,
    body: Bytes,
    kind: ResultKind,
) -> StatusCode {
    *lock(&s.current) = None;
    if let Some(tx) = lock(&s.results).remove(&id) {
        let _ = tx.send(InvocationResult {
            kind,
            body: body.to_vec(),
        });
    }
    StatusCode::ACCEPTED
}

async fn init_error(State(s): State<Arc<Shared>>, body: Bytes) -> StatusCode {
    *lock(&s.init_error) = Some(String::from_utf8_lossy(&body).into_owned());
    StatusCode::ACCEPTED
}

async fn register(State(s): State<Arc<Shared>>) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Lambda-Extension-Identifier",
        HeaderValue::from_static(EXTENSION_ID),
    );
    (
        StatusCode::OK,
        headers,
        Json(json!({
            "functionName": s.function_name,
            "functionVersion": "$LATEST",
            "handler": s.handler,
            "accountId": "123456789012",
        })),
    )
}

async fn extension_next(State(s): State<Arc<Shared>>) -> Response {
    s.extension_next_calls.send_modify(|n| *n += 1);
    let mut rx = s.extension_events.lock().await;
    match rx.recv().await {
        Some(event) => Json(event).into_response(),
        None => {
            drop(rx);
            std::future::pending::<()>().await;
            unreachable!();
        }
    }
}

async fn subscribe(State(s): State<Arc<Shared>>, body: Bytes) -> StatusCode {
    let uri = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v["destination"]["URI"].as_str().map(ToString::to_string));
    *lock(&s.telemetry_uri) = uri;
    StatusCode::OK
}

async fn intake(State(s): State<Arc<Shared>>, uri: axum::http::Uri, body: Bytes) -> StatusCode {
    lock(&s.intake).push(IntakeRequest {
        path: uri.path().to_string(),
        body: body.to_vec(),
    });
    StatusCode::ACCEPTED
}
