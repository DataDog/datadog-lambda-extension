// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! In-process fake Datadog intake for APM payload-level integration tests and
//! local debugging.
//!
//! Spawns an axum server on a local port that accepts the same APM endpoints
//! bottlecap flushes to, decodes msgpack / protobuf payloads on arrival, and
//! stores the decoded structs. Callers then use typed query methods to assert
//! on payload contents.
//!
//! Endpoints supported:
//!
//! - `POST /api/v0.2/stats`: msgpack, gzip-compressed, `pb::StatsPayload`
//! - `POST /api/v0.2/traces`: protobuf (optionally zstd-compressed), `pb::AgentPayload`
//! - `POST /api/v0.1/pipeline_stats`: msgpack (struct-as-map), gzip-compressed, DSM pipeline stats
//!
//! Two ways to use this module:
//!
//! - **Embedded**: `FakeIntake::start()` binds `127.0.0.1:0` with defaults and
//!   is used by the APM / DSM integration tests in `tests/`.
//! - **Standalone binary**: `cargo run --bin fake-intake --features fake-intake`
//!   runs this module with request summaries, optional stats failure injection
//!   (`FAKE_INTAKE_FAIL_STATS_FIRST_N`), and optional JSON dumps
//!   (`FAKE_INTAKE_DUMP_DIR`), for local debugging against a live tracer or the
//!   test-mode trace processor. See `bottlecap/README.md` for the workflow.
//!
//! This module is self-contained (no Bottlecap config, logging, or trace
//! processing dependencies) so it can later be extracted into the shared
//! `datadog/apm-agent-parity-rs` repo.
//!
//! DSM JSON dumps contain only the fields this fixture decodes
//! (`PipelineStatsPayload` below); serde ignores the rest of the wire payload,
//! including the `serde_bytes` latency sketches.

use std::fmt::Write as _;
use std::io::Read;
use std::path::PathBuf;
use std::process;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use libdd_trace_protobuf::pb;
use prost::Message;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// A DSM pipeline-stats payload as it lands on `/api/v0.1/pipeline_stats`.
/// Only the fields tests assert on are decoded; serde ignores the rest
/// (including the `serde_bytes` latency sketches). JSON dumps therefore
/// contain only these fields, not the full wire payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PipelineStatsPayload {
    #[serde(rename = "Env")]
    pub env: String,
    #[serde(rename = "Service")]
    pub service: String,
    #[serde(rename = "TracerVersion")]
    pub tracer_version: String,
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Tags")]
    pub tags: Vec<String>,
    #[serde(rename = "Stats")]
    pub stats: Vec<PipelineStatsBucket>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PipelineStatsBucket {
    #[serde(rename = "Stats")]
    pub stats: Vec<PipelineStatsPoint>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PipelineStatsPoint {
    #[serde(rename = "Hash")]
    pub hash: u64,
    #[serde(rename = "ParentHash")]
    pub parent_hash: u64,
    #[serde(rename = "EdgeTags")]
    pub edge_tags: Vec<String>,
}

/// Options controlling a standalone fake-intake server. The embedded default
/// (`FakeIntakeOptions::default()`) disables all diagnostics.
#[derive(Clone, Debug, Default)]
pub struct FakeIntakeOptions {
    /// Port to bind on `127.0.0.1`. `0` lets the OS assign a free port; the
    /// actual address is available via `FakeIntake::base_url()`.
    pub port: u16,
    /// Emit one summary line per handled request to stderr.
    pub request_summaries: bool,
    /// Return HTTP 500 for the first N stats request attempts. Attempts are
    /// still decoded so they can be summarized and dumped, but rejected
    /// payloads are not captured by `stats_payloads()`.
    pub fail_stats_first_n: usize,
    /// Write one JSON envelope per successfully decoded request attempt
    /// (including rejected stats attempts) into this directory.
    pub dump_dir: Option<PathBuf>,
}

/// Errors from `FakeIntake::start_with_options`.
#[derive(Debug, thiserror::Error)]
pub enum FakeIntakeError {
    #[error("fake_intake: failed to bind listener on 127.0.0.1:{port}: {source}")]
    Bind {
        port: u16,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "fake_intake: failed to initialize dump directory {}: {source}",
        .dir.display()
    )]
    DumpDir {
        dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Captured, decoded APM payloads for a single test run.
#[derive(Debug, Default)]
struct Captured {
    stats: Vec<pb::StatsPayload>,
    traces: Vec<pb::AgentPayload>,
    pipeline_stats: Vec<PipelineStatsPayload>,
}

/// Shared server state. The axum handlers write to the mutex; callers read
/// via `FakeIntake::stats_payloads()` / `trace_payloads()`.
#[derive(Debug)]
struct SharedState {
    captured: Mutex<Captured>,
    options: FakeIntakeOptions,
    /// Monotonic request identity for summaries and dumps.
    request_counter: AtomicU64,
    /// Stats request attempts, used for `fail_stats_first_n` injection.
    stats_attempts: AtomicU64,
}

/// A running fake-intake server. Drop shuts it down.
#[derive(Debug)]
pub struct FakeIntake {
    base_url: String,
    state: std::sync::Arc<SharedState>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl FakeIntake {
    /// Bind to `127.0.0.1` on an OS-assigned port and start serving.
    pub async fn start() -> Self {
        Self::start_with_options(FakeIntakeOptions::default())
            .await
            .expect("fake_intake: failed to start server on 127.0.0.1:0")
    }

    /// Start serving with explicit options. Fails on bind errors or when the
    /// dump directory cannot be created; nothing is served otherwise.
    pub async fn start_with_options(options: FakeIntakeOptions) -> Result<Self, FakeIntakeError> {
        if let Some(dir) = &options.dump_dir {
            std::fs::create_dir_all(dir).map_err(|source| FakeIntakeError::DumpDir {
                dir: dir.clone(),
                source,
            })?;
        }

        let listener = TcpListener::bind(("127.0.0.1", options.port))
            .await
            .map_err(|source| FakeIntakeError::Bind {
                port: options.port,
                source,
            })?;
        let addr = listener
            .local_addr()
            .map_err(|source| FakeIntakeError::Bind {
                port: options.port,
                source,
            })?;
        let base_url = format!("http://{addr}");

        let state = std::sync::Arc::new(SharedState {
            captured: Mutex::new(Captured::default()),
            options,
            request_counter: AtomicU64::new(0),
            stats_attempts: AtomicU64::new(0),
        });

        let router = Router::new()
            .route("/api/v0.2/stats", post(handle_stats))
            .route("/api/v0.2/traces", post(handle_traces))
            .route("/api/v0.1/pipeline_stats", post(handle_pipeline_stats))
            .with_state(std::sync::Arc::clone(&state));

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("fake_intake: axum server error");
        });

        Ok(Self {
            base_url,
            state,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        })
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

    /// Base URL (scheme + host + port, no path). Use as the `apm_dd_url` for
    /// components that build their own endpoint path (e.g. `DsmProcessor`).
    #[must_use]
    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    /// All DSM pipeline-stats payloads captured so far, in arrival order.
    #[must_use]
    pub fn pipeline_stats_payloads(&self) -> Vec<PipelineStatsPayload> {
        self.state
            .captured
            .lock()
            .expect("fake_intake: pipeline_stats mutex poisoned")
            .pipeline_stats
            .clone()
    }

    /// All `StatsPayload`s captured so far, in arrival order. Rejected stats
    /// attempts (failure injection) are excluded.
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

/// Result of handling one intake request, before summary and dump emission.
struct HandledRequest<T> {
    request_id: u64,
    status: StatusCode,
    decoded: Option<T>,
}

async fn handle_stats(
    State(state): State<std::sync::Arc<SharedState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let request_id = next_request_id(&state);
    let attempt = state.stats_attempts.fetch_add(1, Ordering::SeqCst) + 1;
    let inject_failure = attempt <= state.options.fail_stats_first_n as u64;

    let handled: HandledRequest<pb::StatsPayload> = match decompress(&headers, &body) {
        Ok(d) => match rmp_serde::from_slice::<pb::StatsPayload>(&d) {
            Ok(payload) => {
                let status = if inject_failure {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    state
                        .captured
                        .lock()
                        .expect("fake_intake: stats mutex poisoned")
                        .stats
                        .push(payload.clone());
                    StatusCode::ACCEPTED
                };
                HandledRequest {
                    request_id,
                    status,
                    decoded: Some(payload),
                }
            }
            Err(err) => {
                eprintln!("fake_intake: failed to decode StatsPayload msgpack: {err}");
                HandledRequest {
                    request_id,
                    status: failure_status(inject_failure),
                    decoded: None,
                }
            }
        },
        Err(e) => {
            eprintln!("{e}");
            HandledRequest {
                request_id,
                status: failure_status(inject_failure),
                decoded: None,
            }
        }
    };

    let payload_count = handled.decoded.as_ref().map_or(0, |p| p.stats.len());

    if state.options.request_summaries {
        let groups = handled.decoded.as_ref().map(stats_hits_by_key);
        log_summary(
            handled.request_id,
            "/api/v0.2/stats",
            &headers,
            handled.status,
            payload_count,
            groups.as_ref(),
        );
    }

    if let Some(payload) = &handled.decoded {
        dump_request(
            &state,
            handled.request_id,
            "/api/v0.2/stats",
            &headers,
            handled.status,
            serde_json::to_value(payload).unwrap_or_else(|_| serde_json::Value::Null),
        );
    }

    handled.status
}

async fn handle_traces(
    State(state): State<std::sync::Arc<SharedState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let request_id = next_request_id(&state);
    let handled: HandledRequest<pb::AgentPayload> = match decompress(&headers, &body) {
        Ok(d) => match pb::AgentPayload::decode(d.as_slice()) {
            Ok(payload) => {
                state
                    .captured
                    .lock()
                    .expect("fake_intake: traces mutex poisoned")
                    .traces
                    .push(payload.clone());
                HandledRequest {
                    request_id,
                    status: StatusCode::ACCEPTED,
                    decoded: Some(payload),
                }
            }
            Err(err) => {
                eprintln!("fake_intake: failed to decode AgentPayload protobuf: {err}");
                HandledRequest {
                    request_id,
                    status: StatusCode::BAD_REQUEST,
                    decoded: None,
                }
            }
        },
        Err(e) => {
            eprintln!("{e}");
            HandledRequest {
                request_id,
                status: StatusCode::BAD_REQUEST,
                decoded: None,
            }
        }
    };

    // Tracer payload count across the ordinary and indexed collections.
    let payload_count = handled
        .decoded
        .as_ref()
        .map_or(0, |p| p.tracer_payloads.len() + p.idx_tracer_payloads.len());

    if state.options.request_summaries {
        log_summary(
            handled.request_id,
            "/api/v0.2/traces",
            &headers,
            handled.status,
            payload_count,
            None,
        );
    }

    if let Some(payload) = &handled.decoded {
        dump_request(
            &state,
            handled.request_id,
            "/api/v0.2/traces",
            &headers,
            handled.status,
            agent_payload_to_json(payload),
        );
    }

    handled.status
}

async fn handle_pipeline_stats(
    State(state): State<std::sync::Arc<SharedState>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let request_id = next_request_id(&state);
    let handled: HandledRequest<PipelineStatsPayload> = match decompress(&headers, &body) {
        Ok(d) => match rmp_serde::from_slice::<PipelineStatsPayload>(&d) {
            Ok(payload) => {
                state
                    .captured
                    .lock()
                    .expect("fake_intake: pipeline_stats mutex poisoned")
                    .pipeline_stats
                    .push(payload.clone());
                HandledRequest {
                    request_id,
                    status: StatusCode::ACCEPTED,
                    decoded: Some(payload),
                }
            }
            Err(err) => {
                eprintln!("fake_intake: failed to decode pipeline stats msgpack: {err}");
                HandledRequest {
                    request_id,
                    status: StatusCode::BAD_REQUEST,
                    decoded: None,
                }
            }
        },
        Err(e) => {
            eprintln!("{e}");
            HandledRequest {
                request_id,
                status: StatusCode::BAD_REQUEST,
                decoded: None,
            }
        }
    };

    if state.options.request_summaries {
        log_summary(
            handled.request_id,
            "/api/v0.1/pipeline_stats",
            &headers,
            handled.status,
            handled.decoded.as_ref().map_or(0, |_| 1),
            None,
        );
    }

    if let Some(payload) = &handled.decoded {
        dump_request(
            &state,
            handled.request_id,
            "/api/v0.1/pipeline_stats",
            &headers,
            handled.status,
            serde_json::to_value(payload).unwrap_or_else(|_| serde_json::Value::Null),
        );
    }

    handled.status
}

/// Status used for a stats request that failed to decode or was rejected by
/// failure injection. Decode failures outside the injection window keep the
/// historical `400 Bad Request` behavior.
fn failure_status(inject_failure: bool) -> StatusCode {
    if inject_failure {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::BAD_REQUEST
    }
}

fn next_request_id(state: &SharedState) -> u64 {
    state.request_counter.fetch_add(1, Ordering::SeqCst) + 1
}

fn content_encoding(headers: &HeaderMap) -> String {
    headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| "identity".to_string(), ToString::to_string)
}

/// Emit one summary line per handled request. Payload contents stay out of
/// logs; for stats, only aggregated hits per full grouping key are shown.
fn log_summary(
    request_id: u64,
    endpoint: &str,
    headers: &HeaderMap,
    status: StatusCode,
    payload_count: usize,
    groups: Option<&std::collections::BTreeMap<StatsGroupKey, u64>>,
) {
    let mut line = format!(
        "fake-intake: request {request_id} | POST {endpoint} | encoding={} | status={} | payloads={payload_count}",
        content_encoding(headers),
        status.as_u16(),
    );
    if let Some(groups) = groups {
        let rendered: Vec<String> = groups
            .iter()
            .map(|(key, hits)| format!("hits={hits} {}", key.render()))
            .collect();
        let _ = write!(line, " | groups={} | {}", groups.len(), rendered.join("; "));
    }
    eprintln!("{line}");
}

/// Serialize a JSON envelope for one decoded request attempt and write it to
/// the dump directory. Diagnostic only: failures are reported but never
/// change the intake response. No capture lock is held during serialization
/// or filesystem I/O.
fn dump_request(
    state: &SharedState,
    request_id: u64,
    endpoint: &str,
    headers: &HeaderMap,
    status: StatusCode,
    payload: serde_json::Value,
) {
    let Some(dump_dir) = &state.options.dump_dir else {
        return;
    };

    let envelope = serde_json::json!({
        "request_id": request_id,
        "endpoint": endpoint,
        "encoding": content_encoding(headers),
        "status": status.as_u16(),
        "payload": payload,
    });
    let Ok(mut json) = serde_json::to_vec_pretty(&envelope) else {
        eprintln!("fake_intake: failed to serialize dump envelope for request {request_id}");
        return;
    };
    json.push(b'\n');

    // Collision-resistant filename: slug + request id + pid + nanos. A final
    // uniqueness check with create_new guarantees no earlier dump is ever
    // overwritten, even across process restarts.
    let slug = endpoint.replace('/', "_");
    let base = format!(
        "{slug}-{request_id:04}-{}-{}",
        process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let mut suffix = 0;
    loop {
        let path = if suffix == 0 {
            dump_dir.join(format!("{base}.json"))
        } else {
            dump_dir.join(format!("{base}-{suffix}.json"))
        };
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(e) = std::io::Write::write_all(&mut file, &json) {
                    eprintln!("fake_intake: failed to write dump {}: {e}", path.display());
                }
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                suffix += 1;
                if suffix > 1000 {
                    eprintln!(
                        "fake_intake: giving up on dump filename for request {request_id} after 1000 collisions"
                    );
                    return;
                }
            }
            Err(e) => {
                eprintln!("fake_intake: failed to create dump {}: {e}", path.display());
                return;
            }
        }
    }
}

/// Manual JSON serialization for `pb::AgentPayload`, which does not implement
/// `Serialize` in `libdd-trace-protobuf` 4.0.1. Its tracer-payload collections
/// do, so the fields are embedded directly. Fields mirror the protobuf tags.
fn agent_payload_to_json(payload: &pb::AgentPayload) -> serde_json::Value {
    serde_json::json!({
        "host_name": payload.host_name,
        "env": payload.env,
        "tracer_payloads": payload.tracer_payloads,
        "tags": payload.tags,
        "agent_version": payload.agent_version,
        "target_tps": payload.target_tps,
        "error_tps": payload.error_tps,
        "rare_sampler_enabled": payload.rare_sampler_enabled,
        "idx_tracer_payloads": payload.idx_tracer_payloads,
    })
}

// ---------------------------------------------------------------------------
// Stats grouping
// ---------------------------------------------------------------------------

/// Full aggregation-dimension key for one `ClientGroupedStats` entry.
///
/// Includes every dimension stats aggregation distinguishes: client identity
/// and origin context plus the grouped-stat dimensions. Excludes
/// measurements and delivery metadata (hits, errors, duration, sketches,
/// top-level hits, runtime ID, sequence, tracer version/language) and time
/// bucket fields, so identical dimensions combine across time windows.
///
/// Tag lists are normalized (sorted) here for deterministic summaries;
/// captured payloads are never modified.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct StatsGroupKey {
    // Client dimensions (from `ClientStatsPayload`).
    client_service: String,
    client_env: String,
    client_version: String,
    client_hostname: String,
    client_container_id: String,
    client_tags: Vec<String>,
    client_git_commit_sha: String,
    client_image_tag: String,
    client_process_tags: String,
    client_process_tags_hash: u64,
    client_agent_aggregation: String,

    // Grouped dimensions (from `ClientGroupedStats`).
    stats_service: String,
    stats_name: String,
    stats_resource: String,
    stats_type: String,
    stats_db_type: String,
    stats_http_status_code: u32,
    stats_grpc_status_code: String,
    stats_http_method: String,
    stats_http_endpoint: String,
    stats_span_kind: String,
    stats_synthetics: bool,
    stats_is_trace_root: i32,
    stats_service_source: String,
    stats_peer_tags: Vec<String>,
    stats_span_derived_primary_tags: Vec<String>,
    stats_additional_metric_tags: Vec<String>,
}

impl StatsGroupKey {
    fn new(client: &pb::ClientStatsPayload, grouped: &pb::ClientGroupedStats) -> Self {
        let sorted = |mut tags: Vec<String>| {
            tags.sort();
            tags
        };
        Self {
            client_service: client.service.clone(),
            client_env: client.env.clone(),
            client_version: client.version.clone(),
            client_hostname: client.hostname.clone(),
            client_container_id: client.container_id.clone(),
            client_tags: sorted(client.tags.clone()),
            client_git_commit_sha: client.git_commit_sha.clone(),
            client_image_tag: client.image_tag.clone(),
            client_process_tags: client.process_tags.clone(),
            client_process_tags_hash: client.process_tags_hash,
            client_agent_aggregation: client.agent_aggregation.clone(),

            stats_service: grouped.service.clone(),
            stats_name: grouped.name.clone(),
            stats_resource: grouped.resource.clone(),
            stats_type: grouped.r#type.clone(),
            stats_db_type: grouped.db_type.clone(),
            stats_http_status_code: grouped.http_status_code,
            stats_grpc_status_code: grouped.grpc_status_code.clone(),
            stats_http_method: grouped.http_method.clone(),
            stats_http_endpoint: grouped.http_endpoint.clone(),
            stats_span_kind: grouped.span_kind.clone(),
            stats_synthetics: grouped.synthetics,
            stats_is_trace_root: grouped.is_trace_root,
            stats_service_source: grouped.service_source.clone(),
            stats_peer_tags: sorted(grouped.peer_tags.clone()),
            stats_span_derived_primary_tags: sorted(grouped.span_derived_primary_tags.clone()),
            stats_additional_metric_tags: sorted(grouped.additional_metric_tags.clone()),
        }
    }

    /// Stable, escaped one-line representation in a fixed field order.
    fn render(&self) -> String {
        let mut fields: Vec<String> = Vec::new();
        fields.push(format!("client.service={}", escape(&self.client_service)));
        fields.push(format!("client.env={}", escape(&self.client_env)));
        fields.push(format!("client.version={}", escape(&self.client_version)));
        fields.push(format!("client.hostname={}", escape(&self.client_hostname)));
        fields.push(format!(
            "client.container_id={}",
            escape(&self.client_container_id)
        ));
        fields.push(format!("client.tags={}", render_tags(&self.client_tags)));
        fields.push(format!(
            "client.git_commit_sha={}",
            escape(&self.client_git_commit_sha)
        ));
        fields.push(format!(
            "client.image_tag={}",
            escape(&self.client_image_tag)
        ));
        fields.push(format!(
            "client.process_tags={}",
            escape(&self.client_process_tags)
        ));
        fields.push(format!(
            "client.process_tags_hash={}",
            self.client_process_tags_hash
        ));
        fields.push(format!(
            "client.agent_aggregation={}",
            escape(&self.client_agent_aggregation)
        ));

        fields.push(format!("stats.service={}", escape(&self.stats_service)));
        fields.push(format!("stats.name={}", escape(&self.stats_name)));
        fields.push(format!("stats.resource={}", escape(&self.stats_resource)));
        fields.push(format!("stats.type={}", escape(&self.stats_type)));
        fields.push(format!("stats.db_type={}", escape(&self.stats_db_type)));
        fields.push(format!(
            "stats.http_status_code={}",
            self.stats_http_status_code
        ));
        fields.push(format!(
            "stats.grpc_status_code={}",
            escape(&self.stats_grpc_status_code)
        ));
        fields.push(format!(
            "stats.http_method={}",
            escape(&self.stats_http_method)
        ));
        fields.push(format!(
            "stats.http_endpoint={}",
            escape(&self.stats_http_endpoint)
        ));
        fields.push(format!("stats.span_kind={}", escape(&self.stats_span_kind)));
        fields.push(format!("stats.synthetics={}", self.stats_synthetics));
        fields.push(format!(
            "stats.is_trace_root={}",
            escape(&trilean_name(self.stats_is_trace_root))
        ));
        fields.push(format!(
            "stats.service_source={}",
            escape(&self.stats_service_source)
        ));
        fields.push(format!(
            "stats.peer_tags={}",
            render_tags(&self.stats_peer_tags)
        ));
        fields.push(format!(
            "stats.span_derived_primary_tags={}",
            render_tags(&self.stats_span_derived_primary_tags)
        ));
        fields.push(format!(
            "stats.additional_metric_tags={}",
            render_tags(&self.stats_additional_metric_tags)
        ));
        fields.join(" ")
    }
}

/// JSON-escape a value for stable output in summaries.
fn escape(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// Render a sorted tag list as `["a:1","b:2"]` for summaries.
fn render_tags(tags: &[String]) -> String {
    let rendered: Vec<String> = tags.iter().map(|t| escape(t)).collect();
    format!("[{}]", rendered.join(","))
}

fn trilean_name(value: i32) -> String {
    match pb::Trilean::try_from(value) {
        Ok(t) => t.as_str_name().to_string(),
        Err(_) => value.to_string(),
    }
}

/// Sum `hits` across client payloads and time buckets, grouped by the full
/// aggregation key. Returned as a `BTreeMap` for deterministic ordering.
fn stats_hits_by_key(payload: &pb::StatsPayload) -> std::collections::BTreeMap<StatsGroupKey, u64> {
    let mut groups: std::collections::BTreeMap<StatsGroupKey, u64> =
        std::collections::BTreeMap::new();
    for client in &payload.stats {
        for bucket in &client.stats {
            for grouped in &bucket.stats {
                *groups
                    .entry(StatsGroupKey::new(client, grouped))
                    .or_insert(0) += grouped.hits;
            }
        }
    }
    groups
}

// ---------------------------------------------------------------------------
// Body decoding
// ---------------------------------------------------------------------------

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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {

    use super::*;

    /// POST a raw body to a path on the intake with an optional
    /// Content-Encoding header. Returns the response status code.
    async fn post(base_url: &str, path: &str, encoding: Option<&str>, body: Vec<u8>) -> StatusCode {
        let client = reqwest::Client::new();
        let url = format!("{base_url}{path}");
        let mut request = client.post(&url);
        if let Some(enc) = encoding {
            request = request.header("content-encoding", enc);
        }
        request
            .header("content-type", "application/msgpack")
            .body(body)
            .send()
            .await
            .expect("test: request to fake intake failed")
            .status()
    }

    fn gzip(data: Vec<u8>) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &data).expect("test: gzip compression failed");
        encoder.finish().expect("test: gzip compression failed")
    }

    /// Build a `pb::StatsPayload` with one client payload, one bucket, and the
    /// given grouped stats entries.
    fn stats_payload(
        client: pb::ClientStatsPayload,
        start: u64,
        grouped: Vec<pb::ClientGroupedStats>,
    ) -> pb::StatsPayload {
        pb::StatsPayload {
            stats: vec![pb::ClientStatsPayload {
                stats: vec![pb::ClientStatsBucket {
                    start,
                    duration: 10_000_000_000,
                    stats: grouped,
                    agent_time_shift: 0,
                }],
                ..client
            }],
            ..pb::StatsPayload::default()
        }
    }

    fn grouped_stats(
        service: &str,
        resource: &str,
        hits: u64,
        overrides: impl FnOnce(&mut pb::ClientGroupedStats),
    ) -> pb::ClientGroupedStats {
        let mut g = pb::ClientGroupedStats {
            service: service.to_string(),
            name: "smoke.request".to_string(),
            resource: resource.to_string(),
            r#type: "web".to_string(),
            span_kind: "server".to_string(),
            http_status_code: 200,
            is_trace_root: pb::Trilean::True as i32,
            hits,
            ..pb::ClientGroupedStats::default()
        };
        overrides(&mut g);
        g
    }

    fn client_payload(env: &str) -> pb::ClientStatsPayload {
        pb::ClientStatsPayload {
            service: "fake-intake-smoke".to_string(),
            env: env.to_string(),
            version: "smoke".to_string(),
            ..pb::ClientStatsPayload::default()
        }
    }

    async fn start_default() -> FakeIntake {
        FakeIntake::start().await
    }

    #[tokio::test]
    async fn default_startup_binds_port_zero_and_exposes_url_helpers() {
        let intake = start_default().await;
        assert!(intake.base_url().starts_with("http://127.0.0.1:"));
        assert!(intake.stats_url().ends_with("/api/v0.2/stats"));
        assert!(intake.traces_url().ends_with("/api/v0.2/traces"));
    }

    #[tokio::test]
    async fn instances_are_independent() {
        let a = start_default().await;
        let b = start_default().await;
        assert_ne!(a.base_url(), b.base_url());

        let payload = stats_payload(
            client_payload("local"),
            1,
            vec![grouped_stats("svc", "GET /a", 1, |_| {})],
        );
        let body = rmp_serde::to_vec_named(&payload).expect("test: msgpack encode failed");
        assert_eq!(
            post(&a.base_url(), "/api/v0.2/stats", None, body).await,
            StatusCode::ACCEPTED
        );
        assert_eq!(a.stats_payloads().len(), 1);
        assert!(b.stats_payloads().is_empty());

        // Dropping `a` must not shut down `b`.
        drop(a);
        let client = reqwest::Client::new();
        // Any HTTP response proves `b` is still listening; an unmatched route
        // answers 404.
        let response = client
            .get(format!("{}/", b.base_url()))
            .send()
            .await
            .expect("test: intake b should still be listening");
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn drop_shuts_down_server() {
        let intake = start_default().await;
        let url = intake.base_url();
        drop(intake);
        // The listener socket is released on shutdown; a fresh bind on the
        // same port must succeed shortly afterwards.
        let port = url.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
        let port = port.expect("test: no port in base url");
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        let mut bound = false;
        while tokio::time::Instant::now() < deadline {
            if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
                bound = true;
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
        assert!(bound, "listener port was not released after Drop");
    }

    #[tokio::test]
    async fn stats_endpoint_decodes_msgpack_and_gzip() {
        let intake = start_default().await;
        let payload = stats_payload(
            client_payload("local"),
            42,
            vec![grouped_stats("svc", "GET /a", 3, |_| {})],
        );
        let raw = rmp_serde::to_vec_named(&payload).expect("test: msgpack encode failed");

        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.2/stats",
                Some("gzip"),
                gzip(raw.clone())
            )
            .await,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            post(&intake.base_url(), "/api/v0.2/stats", None, raw).await,
            StatusCode::ACCEPTED
        );

        let captured = intake.stats_payloads();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].stats[0].stats[0].stats[0].hits, 3);
        assert_eq!(captured[0].stats[0].stats[0].start, 42);
    }

    #[tokio::test]
    async fn traces_endpoint_decodes_protobuf() {
        let intake = start_default().await;
        let payload = pb::AgentPayload {
            env: "local".to_string(),
            tracer_payloads: vec![pb::TracerPayload::default()],
            ..pb::AgentPayload::default()
        };
        let body = payload.encode_to_vec();
        assert_eq!(
            post(&intake.base_url(), "/api/v0.2/traces", None, body).await,
            StatusCode::ACCEPTED
        );
        let captured = intake.trace_payloads();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].env, "local");
        assert_eq!(captured[0].tracer_payloads.len(), 1);
    }

    #[tokio::test]
    async fn dsm_endpoint_decodes_msgpack_gzip() {
        let intake = start_default().await;
        let payload = PipelineStatsPayload {
            env: "local".to_string(),
            service: "svc".to_string(),
            tracer_version: "1.0".to_string(),
            version: "2.0".to_string(),
            tags: vec!["a:b".to_string()],
            stats: vec![PipelineStatsBucket {
                stats: vec![PipelineStatsPoint {
                    hash: 7,
                    parent_hash: 0,
                    edge_tags: vec!["direction:out".to_string()],
                }],
            }],
        };
        let body = gzip(rmp_serde::to_vec_named(&payload).expect("test: msgpack encode failed"));
        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.1/pipeline_stats",
                Some("gzip"),
                body
            )
            .await,
            StatusCode::ACCEPTED
        );
        let captured = intake.pipeline_stats_payloads();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].stats[0].stats[0].hash, 7);
    }

    #[tokio::test]
    async fn malformed_bodies_return_400_and_are_not_captured() {
        let intake = start_default().await;
        for path in [
            "/api/v0.2/stats",
            "/api/v0.2/traces",
            "/api/v0.1/pipeline_stats",
        ] {
            assert_eq!(
                post(&intake.base_url(), path, None, vec![0xFF; 16]).await,
                StatusCode::BAD_REQUEST,
                "malformed body on {path} should return 400"
            );
        }
        assert!(intake.stats_payloads().is_empty());
        assert!(intake.trace_payloads().is_empty());
        assert!(intake.pipeline_stats_payloads().is_empty());
    }

    #[tokio::test]
    async fn fail_stats_first_n_rejects_exactly_n_then_accepts() {
        let intake = FakeIntake::start_with_options(FakeIntakeOptions {
            fail_stats_first_n: 2,
            ..FakeIntakeOptions::default()
        })
        .await
        .expect("test: start failed");

        let payload = stats_payload(
            client_payload("local"),
            1,
            vec![grouped_stats("svc", "GET /a", 1, |_| {})],
        );
        let body = rmp_serde::to_vec_named(&payload).expect("test: msgpack encode failed");

        assert_eq!(
            post(&intake.base_url(), "/api/v0.2/stats", None, body.clone()).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            post(&intake.base_url(), "/api/v0.2/stats", None, body.clone()).await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            post(&intake.base_url(), "/api/v0.2/stats", None, body).await,
            StatusCode::ACCEPTED
        );

        // Rejected attempts are not captured; only the successful retry is.
        let captured = intake.stats_payloads();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].stats[0].stats[0].stats[0].hits, 1);
    }

    #[tokio::test]
    async fn concurrent_stats_requests_respect_rejection_limit() {
        let intake = FakeIntake::start_with_options(FakeIntakeOptions {
            fail_stats_first_n: 3,
            ..FakeIntakeOptions::default()
        })
        .await
        .expect("test: start failed");

        let payload = stats_payload(
            client_payload("local"),
            1,
            vec![grouped_stats("svc", "GET /a", 1, |_| {})],
        );
        let body = rmp_serde::to_vec_named(&payload).expect("test: msgpack encode failed");

        let client = reqwest::Client::new();
        let url = format!("{}/api/v0.2/stats", intake.base_url());
        let mut handles = Vec::new();
        for _ in 0..10 {
            let client = client.clone();
            let url = url.clone();
            let body = body.clone();
            handles.push(tokio::spawn(async move {
                client
                    .post(&url)
                    .body(body)
                    .send()
                    .await
                    .expect("test: request failed")
                    .status()
                    .as_u16()
            }));
        }
        let mut rejected = 0;
        let mut accepted = 0;
        for handle in handles {
            match handle.await.expect("test: task panicked") {
                500 => rejected += 1,
                202 => accepted += 1,
                other => panic!("unexpected status {other}"),
            }
        }
        assert_eq!(rejected, 3, "exactly the first 3 attempts must be rejected");
        assert_eq!(accepted, 7);
        assert_eq!(intake.stats_payloads().len(), 7);
    }

    #[tokio::test]
    async fn traces_and_dsm_do_not_consume_stats_rejections() {
        let intake = FakeIntake::start_with_options(FakeIntakeOptions {
            fail_stats_first_n: 1,
            ..FakeIntakeOptions::default()
        })
        .await
        .expect("test: start failed");

        let trace = pb::AgentPayload::default().encode_to_vec();
        let dsm = gzip(
            rmp_serde::to_vec_named(&PipelineStatsPayload {
                env: "local".to_string(),
                service: "svc".to_string(),
                tracer_version: "1.0".to_string(),
                version: "2.0".to_string(),
                tags: Vec::new(),
                stats: Vec::new(),
            })
            .expect("test: msgpack encode failed"),
        );

        assert_eq!(
            post(&intake.base_url(), "/api/v0.2/traces", None, trace).await,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.1/pipeline_stats",
                Some("gzip"),
                dsm
            )
            .await,
            StatusCode::ACCEPTED
        );
        // The stats rejection budget is untouched by the two requests above.
        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.2/stats",
                None,
                rmp_serde::to_vec_named(&stats_payload(
                    client_payload("local"),
                    1,
                    vec![grouped_stats("svc", "GET /a", 1, |_| {})]
                ))
                .expect("test: msgpack encode failed")
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn grouping_combines_across_time_buckets_and_clients() {
        let payload = pb::StatsPayload {
            stats: vec![
                pb::ClientStatsPayload {
                    stats: vec![
                        pb::ClientStatsBucket {
                            start: 100,
                            duration: 10,
                            stats: vec![
                                grouped_stats("svc", "GET /a", 2, |_| {}),
                                grouped_stats("svc", "GET /b", 1, |_| {}),
                            ],
                            agent_time_shift: 0,
                        },
                        pb::ClientStatsBucket {
                            start: 200,
                            duration: 10,
                            stats: vec![grouped_stats("svc", "GET /a", 3, |_| {})],
                            agent_time_shift: 0,
                        },
                    ],
                    ..client_payload("prod")
                },
                // Same dimensions in a second client payload combine too.
                pb::ClientStatsPayload {
                    stats: vec![pb::ClientStatsBucket {
                        start: 300,
                        duration: 10,
                        stats: vec![grouped_stats("svc", "GET /a", 4, |_| {})],
                        agent_time_shift: 0,
                    }],
                    ..client_payload("prod")
                },
            ],
            ..pb::StatsPayload::default()
        };

        let groups = stats_hits_by_key(&payload);
        assert_eq!(groups.len(), 2, "matching keys must combine across buckets");
        let hits: Vec<u64> = groups.values().copied().collect();
        assert_eq!(hits, vec![2 + 3 + 4, 1]);
    }

    type PayloadMutation = Box<dyn Fn(&mut pb::StatsPayload)>;

    #[test]
    #[allow(clippy::too_many_lines)]
    fn grouping_separates_changed_dimensions() {
        let base = || pb::StatsPayload {
            stats: vec![pb::ClientStatsPayload {
                stats: vec![pb::ClientStatsBucket {
                    start: 1,
                    duration: 1,
                    stats: vec![grouped_stats("svc", "GET /a", 1, |_| {})],
                    agent_time_shift: 0,
                }],
                ..client_payload("prod")
            }],
            ..pb::StatsPayload::default()
        };

        let mut dimensions: Vec<(&str, PayloadMutation)> = Vec::new();
        dimensions.push((
            "resource",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].resource = "GET /b".to_string();
            }),
        ));
        dimensions.push((
            "peer_tags",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].peer_tags = vec!["grpc.target:other".to_string()];
            }),
        ));
        dimensions.push((
            "additional_metric_tags",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].additional_metric_tags = vec!["x:y".to_string()];
            }),
        ));
        dimensions.push((
            "is_trace_root",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].is_trace_root = pb::Trilean::False as i32;
            }),
        ));
        dimensions.push((
            "http_status_code",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].http_status_code = 500;
            }),
        ));
        dimensions.push((
            "grpc_status_code",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].grpc_status_code = "14".to_string();
            }),
        ));
        dimensions.push((
            "client_env",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].env = "staging".to_string();
            }),
        ));
        dimensions.push((
            "client_version",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].version = "2.0".to_string();
            }),
        ));
        dimensions.push((
            "span_kind",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].span_kind = "client".to_string();
            }),
        ));
        dimensions.push((
            "http_method",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].http_method = "POST".to_string();
            }),
        ));
        dimensions.push((
            "http_endpoint",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].http_endpoint = "/api".to_string();
            }),
        ));
        dimensions.push((
            "service_source",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].service_source = "dd.trace".to_string();
            }),
        ));
        dimensions.push((
            "span_derived_primary_tags",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].span_derived_primary_tags = vec!["t:1".to_string()];
            }),
        ));
        dimensions.push((
            "db_type",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].db_type = "postgres".to_string();
            }),
        ));
        dimensions.push((
            "synthetics",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].synthetics = true;
            }),
        ));
        dimensions.push((
            "name",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].name = "other.request".to_string();
            }),
        ));
        dimensions.push((
            "type",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].r#type = "db".to_string();
            }),
        ));
        dimensions.push((
            "client_hostname",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].hostname = "host-1".to_string();
            }),
        ));
        dimensions.push((
            "client_container_id",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].container_id = "abc".to_string();
            }),
        ));
        dimensions.push((
            "client_tags",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].tags = vec!["k:v".to_string()];
            }),
        ));
        dimensions.push((
            "client_git_commit_sha",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].git_commit_sha = "deadbeef".to_string();
            }),
        ));
        dimensions.push((
            "client_image_tag",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].image_tag = "v1".to_string();
            }),
        ));
        dimensions.push((
            "client_process_tags",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].process_tags = "proc:a".to_string();
            }),
        ));
        dimensions.push((
            "client_process_tags_hash",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].process_tags_hash = 99;
            }),
        ));
        dimensions.push((
            "client_agent_aggregation",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].agent_aggregation = "counts".to_string();
            }),
        ));
        dimensions.push((
            "stats_service",
            Box::new(|p: &mut pb::StatsPayload| {
                p.stats[0].stats[0].stats[0].service = "other-svc".to_string();
            }),
        ));

        let baseline = stats_hits_by_key(&base());
        assert_eq!(baseline.len(), 1);
        for (name, mutate) in dimensions {
            let mut payload = base();
            mutate(&mut payload);
            let groups = stats_hits_by_key(&payload);
            assert_eq!(
                groups.len(),
                1,
                "{name} change must not merge with baseline"
            );
            assert_ne!(
                groups.keys().next(),
                baseline.keys().next(),
                "{name} must change the grouping key"
            );
        }
    }

    #[test]
    fn grouping_ignores_measurements_and_delivery_metadata() {
        let base = grouped_stats("svc", "GET /a", 1, |_| {});
        let variant = grouped_stats("svc", "GET /a", 7, |g| {
            g.errors = 2;
            g.duration = 500;
            g.top_level_hits = 3;
            g.ok_summary = vec![1, 2, 3];
            g.error_summary = vec![4, 5];
        });
        let client_a = client_payload("prod");
        let mut client_b = client_payload("prod");
        client_b.runtime_id = "other".to_string();
        client_b.sequence = 12;
        client_b.lang = "rust".to_string();
        client_b.tracer_version = "9.9".to_string();

        let key_base = StatsGroupKey::new(&client_a, &base);
        let key_variant = StatsGroupKey::new(&client_b, &variant);
        assert_eq!(
            key_base, key_variant,
            "measurement or delivery changes must not split groups"
        );
    }

    #[test]
    fn summary_output_is_deterministic_and_normalizes_tag_order() {
        let mut client = client_payload("prod");
        client.tags = vec!["z:1".to_string(), "a:2".to_string()];
        let mut grouped = grouped_stats("svc", "GET /a", 5, |_| {});
        grouped.peer_tags = vec!["b:x".to_string(), "a:y".to_string()];

        let key = StatsGroupKey::new(&client, &grouped);
        let first = key.render();
        let second = StatsGroupKey::new(&client, &grouped).render();
        assert_eq!(first, second, "rendering must be deterministic");
        assert!(
            first.contains("client.tags=[\"a:2\",\"z:1\"]"),
            "tag lists must be sorted: {first}"
        );
        assert!(
            first.contains("stats.peer_tags=[\"a:y\",\"b:x\"]"),
            "tag lists must be sorted: {first}"
        );
        assert!(!first.contains("hits="), "summary key must exclude hits");
        assert!(first.starts_with("client.service="));
    }

    #[tokio::test]
    async fn json_dumps_parse_for_all_endpoints_and_rejections() {
        let tmp = tempfile::tempdir().expect("test: tempdir failed");
        let intake = FakeIntake::start_with_options(FakeIntakeOptions {
            dump_dir: Some(tmp.path().to_path_buf()),
            fail_stats_first_n: 1,
            ..FakeIntakeOptions::default()
        })
        .await
        .expect("test: start failed");

        let stats = stats_payload(
            client_payload("local"),
            1,
            vec![grouped_stats("svc", "GET /a", 2, |_| {})],
        );
        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.2/stats",
                None,
                rmp_serde::to_vec_named(&stats).expect("test: msgpack encode failed")
            )
            .await,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.2/stats",
                None,
                rmp_serde::to_vec_named(&stats).expect("test: msgpack encode failed")
            )
            .await,
            StatusCode::ACCEPTED
        );

        let trace = pb::AgentPayload {
            env: "local".to_string(),
            tracer_payloads: vec![pb::TracerPayload::default()],
            ..pb::AgentPayload::default()
        };
        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.2/traces",
                None,
                trace.encode_to_vec()
            )
            .await,
            StatusCode::ACCEPTED
        );

        let dsm = PipelineStatsPayload {
            env: "local".to_string(),
            service: "svc".to_string(),
            tracer_version: "1.0".to_string(),
            version: "2.0".to_string(),
            tags: Vec::new(),
            stats: Vec::new(),
        };
        assert_eq!(
            post(
                &intake.base_url(),
                "/api/v0.1/pipeline_stats",
                None,
                rmp_serde::to_vec_named(&dsm).expect("test: msgpack encode failed")
            )
            .await,
            StatusCode::ACCEPTED
        );

        let mut files: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("test: read dump dir failed")
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .collect();
        files.sort();
        assert_eq!(files.len(), 4, "one dump per decoded attempt: {files:?}");

        let mut seen = std::collections::HashSet::new();
        for path in &files {
            let raw = std::fs::read(path).expect("test: read dump failed");
            let envelope: serde_json::Value =
                serde_json::from_slice(&raw).expect("dump envelope must parse as JSON");
            let dumped_endpoint = envelope["endpoint"].as_str().expect("endpoint key missing");
            let dumped_status = envelope["status"].as_u64().expect("status key missing");
            assert!(
                envelope["payload"].is_object(),
                "decoded payload must be present"
            );
            assert!(
                envelope["request_id"].is_u64(),
                "request id must be present"
            );
            assert!(
                envelope["encoding"].as_str() == Some("identity"),
                "absent content-encoding must report identity"
            );
            seen.insert(format!("{dumped_endpoint}/{dumped_status}"));
        }
        // Rejected stats attempt (500) and its successful retry (202) are
        // both visible with their own identities and statuses.
        assert!(seen.contains("/api/v0.2/stats/500"));
        assert!(seen.contains("/api/v0.2/stats/202"));
        assert!(seen.contains("/api/v0.2/traces/202"));
        assert!(seen.contains("/api/v0.1/pipeline_stats/202"));
    }

    #[tokio::test]
    async fn dumps_never_overwrite_earlier_attempts() {
        let tmp = tempfile::tempdir().expect("test: tempdir failed");
        let intake = FakeIntake::start_with_options(FakeIntakeOptions {
            dump_dir: Some(tmp.path().to_path_buf()),
            ..FakeIntakeOptions::default()
        })
        .await
        .expect("test: start failed");

        let body = rmp_serde::to_vec_named(&stats_payload(
            client_payload("local"),
            1,
            vec![grouped_stats("svc", "GET /a", 1, |_| {})],
        ))
        .expect("test: msgpack encode failed");
        for _ in 0..5 {
            assert_eq!(
                post(&intake.base_url(), "/api/v0.2/stats", None, body.clone()).await,
                StatusCode::ACCEPTED
            );
        }

        let files: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("test: read dump dir failed")
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .collect();
        assert_eq!(files.len(), 5, "every attempt gets its own file");
        let mut contents: Vec<String> = files
            .iter()
            .map(|p| std::fs::read_to_string(p).expect("test: read dump failed"))
            .collect();
        contents.sort();
        let unique: std::collections::HashSet<String> = contents.iter().cloned().collect();
        assert_eq!(
            unique.len(),
            5,
            "each dump must have distinct request identity"
        );
    }

    #[tokio::test]
    async fn invalid_options_fail_clearly() {
        // A dump directory whose parent is a file cannot be created.
        let file = tempfile::NamedTempFile::new().expect("test: tempfile failed");
        let err = FakeIntake::start_with_options(FakeIntakeOptions {
            dump_dir: Some(file.path().join("nested")),
            ..FakeIntakeOptions::default()
        })
        .await
        .expect_err("dump dir under a file must fail");
        assert!(matches!(err, FakeIntakeError::DumpDir { .. }));

        // Binding to a port already in use must fail with a bind error.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test: bind failed");
        let port = listener
            .local_addr()
            .expect("test: local addr failed")
            .port();
        let err = FakeIntake::start_with_options(FakeIntakeOptions {
            port,
            ..FakeIntakeOptions::default()
        })
        .await
        .expect_err("bind conflict must fail");
        assert!(matches!(err, FakeIntakeError::Bind { .. }));
    }

    #[tokio::test]
    async fn start_with_options_uses_configured_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("test: bind failed");
        let port = listener
            .local_addr()
            .expect("test: local addr failed")
            .port();
        drop(listener);

        let intake = FakeIntake::start_with_options(FakeIntakeOptions {
            port,
            ..FakeIntakeOptions::default()
        })
        .await
        .expect("test: start failed");
        assert_eq!(intake.base_url(), format!("http://127.0.0.1:{port}"));
    }
}
