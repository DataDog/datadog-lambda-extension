// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! In-process fake Datadog intake for APM payload-level integration tests.
//!
//! Spawns an axum server on a random local port that accepts the same APM
//! endpoints bottlecap flushes to, decodes msgpack / protobuf payloads on
//! arrival, and stores the decoded structs. Tests then call typed query
//! methods to assert on payload contents.
//!
//! Endpoints supported:
//!
//! - `POST /api/v0.2/stats`: msgpack, gzip-compressed, `pb::StatsPayload`
//! - `POST /api/v0.2/traces`: protobuf (optionally zstd-compressed), `pb::AgentPayload`
//!
//! Prototype for APMSVLS-494 phase 1. If the API proves out, this file gets
//! extracted into the shared `datadog/apm-agent-parity-rs` repo in phase 2.

use std::io::Read;
use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use libdd_trace_protobuf::pb;
use prost::Message;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Captured, decoded APM payloads for a single test run.
#[derive(Default)]
struct Captured {
    stats: Vec<pb::StatsPayload>,
    traces: Vec<pb::AgentPayload>,
}

/// Shared server state. The axum handlers write to the mutexes; tests read
/// via `FakeIntake::stats_payloads()` / `trace_payloads()`.
struct SharedState {
    captured: Mutex<Captured>,
}

/// A running fake-intake server. Drop shuts it down.
pub struct FakeIntake {
    base_url: String,
    state: Arc<SharedState>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl FakeIntake {
    /// Bind to `127.0.0.1` on an OS-assigned port and start serving.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake_intake: failed to bind listener");
        let addr = listener
            .local_addr()
            .expect("fake_intake: failed to read bound address");
        let base_url = format!("http://{addr}");

        let state = Arc::new(SharedState {
            captured: Mutex::new(Captured::default()),
        });

        let router = Router::new()
            .route("/api/v0.2/stats", post(handle_stats))
            .route("/api/v0.2/traces", post(handle_traces))
            .with_state(Arc::clone(&state));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("fake_intake: axum server error");
        });

        Self {
            base_url,
            state,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        }
    }

    /// Full URL for the stats endpoint.
    #[must_use]
    pub fn stats_url(&self) -> String {
        format!("{}/api/v0.2/stats", self.base_url)
    }

    /// Full URL for the traces endpoint.
    #[must_use]
    pub fn traces_url(&self) -> String {
        format!("{}/api/v0.2/traces", self.base_url)
    }

    /// All `StatsPayload`s captured so far, in arrival order.
    #[must_use]
    pub fn stats_payloads(&self) -> Vec<pb::StatsPayload> {
        self.state
            .captured
            .lock()
            .expect("fake_intake: stats mutex poisoned")
            .stats
            .clone()
    }

    /// All `AgentPayload`s captured so far, in arrival order.
    #[must_use]
    pub fn trace_payloads(&self) -> Vec<pb::AgentPayload> {
        self.state
            .captured
            .lock()
            .expect("fake_intake: traces mutex poisoned")
            .traces
            .clone()
    }
}

impl Drop for FakeIntake {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn handle_stats(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let decoded = match decompress(&headers, &body) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return StatusCode::BAD_REQUEST;
        }
    };
    match rmp_serde::from_slice::<pb::StatsPayload>(&decoded) {
        Ok(payload) => {
            state
                .captured
                .lock()
                .expect("fake_intake: stats mutex poisoned")
                .stats
                .push(payload);
            StatusCode::ACCEPTED
        }
        Err(err) => {
            eprintln!("fake_intake: failed to decode StatsPayload msgpack: {err}");
            StatusCode::BAD_REQUEST
        }
    }
}

async fn handle_traces(
    State(state): State<Arc<SharedState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let decoded = match decompress(&headers, &body) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return StatusCode::BAD_REQUEST;
        }
    };
    match pb::AgentPayload::decode(decoded.as_slice()) {
        Ok(payload) => {
            state
                .captured
                .lock()
                .expect("fake_intake: traces mutex poisoned")
                .traces
                .push(payload);
            StatusCode::ACCEPTED
        }
        Err(err) => {
            eprintln!("fake_intake: failed to decode AgentPayload protobuf: {err}");
            StatusCode::BAD_REQUEST
        }
    }
}

/// Decompress a request body based on its `Content-Encoding` header.
/// Supports `gzip` and `zstd`. An unknown or absent encoding is treated as
/// identity: the body is returned unchanged.
fn decompress(headers: &HeaderMap, body: &Bytes) -> Result<Vec<u8>, String> {
    let encoding = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    match encoding.as_str() {
        "gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(body.as_ref());
            let mut out = Vec::new();
            decoder
                .read_to_end(&mut out)
                .map_err(|e| format!("fake_intake: gzip decode failed: {e}"))?;
            Ok(out)
        }
        "zstd" => zstd::stream::decode_all(body.as_ref())
            .map_err(|e| format!("fake_intake: zstd decode failed: {e}")),
        _ => {
            if !encoding.is_empty() {
                eprintln!(
                    "fake_intake: unrecognized Content-Encoding '{encoding}', treating as identity"
                );
            }
            Ok(body.to_vec())
        }
    }
}
