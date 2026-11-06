// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! A Datadog intake that misbehaves on demand.
//!
//! Plain HTTP/1.1 on a random local port (set `DD_HTTP_PROTOCOL=http1` on the
//! extension). Every request is recorded with its path, size, headers, arrival
//! time and the mode that answered it; an upload that breaks off is not recorded and
//! gets no answer. The fault mode can change at runtime, and one
//! path can get its own mode (for example `/credentials`, for a secret fetch).
//!
//! Mode names: `ok`, `status:<code>`, `slow:<ms>`, `blackhole`,
//! `retry_after:<code>:<secs>`, `close_mid_body`, and `syn_blackhole` for
//! [`SynBlackhole`]. These modes belong in the shared
//! `datadog-mock-intake` crate once bottlecap consumes it; see `tests/HARNESS.md`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use datadog_protos::metrics::{MetricPayload, SketchPayload};
use futures::stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use libdd_trace_protobuf::pb;
use prost::Message as _;
use protobuf::Message as _;
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::task::JoinHandle;

/// How the intake answers.
#[derive(Clone, Debug, PartialEq)]
pub enum FaultMode {
    /// `202 Accepted` with `{}`.
    Ok,
    /// Always this status, with a plain-text (non-JSON) body.
    Status(u16),
    /// Wait, then `202`.
    Slow(Duration),
    /// Accept the connection, never read, never answer.
    Blackhole,
    /// This status with a `Retry-After: <secs>` header.
    RetryAfter { status: u16, secs: u64 },
    /// Status line and headers, a few body bytes, then close the connection.
    CloseMidBody,
}

impl FaultMode {
    /// The shared string form.
    pub fn name(&self) -> String {
        match self {
            Self::Ok => "ok".into(),
            Self::Status(c) => format!("status:{c}"),
            Self::Slow(d) => format!("slow:{}", d.as_millis()),
            Self::Blackhole => "blackhole".into(),
            Self::RetryAfter { status, secs } => format!("retry_after:{status}:{secs}"),
            Self::CloseMidBody => "close_mid_body".into(),
        }
    }
}

/// One request the intake received.
#[derive(Clone, Debug)]
pub struct IntakeRequest {
    pub method: String,
    pub path: String,
    /// Lowercase header names; the last value wins on repeats.
    pub headers: HashMap<String, String>,
    pub body_len: usize,
    pub body: Vec<u8>,
    /// When the intake had the whole body (the head, for `Blackhole`).
    pub at: Instant,
    /// Mode in force when it was answered.
    pub mode: String,
    pub status: u16,
}

impl IntakeRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn api_key(&self) -> Option<&str> {
        self.header("dd-api-key")
    }

    /// Body, zstd- or gzip-decoded when `Content-Encoding` says so. A body that
    /// does not decode is an error, never partial output.
    pub fn decoded_body(&self) -> Result<Vec<u8>, String> {
        let decoded = match self.header("content-encoding") {
            Some("zstd") => zstd::decode_all(self.body.as_slice()),
            Some("gzip") => {
                use std::io::Read;
                let mut out = Vec::new();
                flate2::read::GzDecoder::new(self.body.as_slice())
                    .read_to_end(&mut out)
                    .map(|_| out)
            }
            _ => Ok(self.body.clone()),
        };
        decoded.map_err(|e| format!("{} {}: undecodable body: {e}", self.method, self.path))
    }

    pub fn body_text(&self) -> Result<String, String> {
        self.decoded_body()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// Trace ID of every span in a `/api/v0.2/traces` body (`AgentPayload`).
    pub fn trace_ids(&self) -> Result<Vec<u64>, String> {
        let payload = pb::AgentPayload::decode(self.decoded_body()?.as_slice()).map_err(|e| {
            format!(
                "{} {}: undecodable AgentPayload: {e}",
                self.method, self.path
            )
        })?;
        Ok(payload
            .tracer_payloads
            .iter()
            .flat_map(|t| &t.chunks)
            .flat_map(|c| &c.spans)
            .map(|span| span.trace_id)
            .collect())
    }

    /// Metric name of every sketch or series in a `/api/beta/sketches` or
    /// `/api/v2/series` body.
    pub fn metric_names(&self) -> Result<Vec<String>, String> {
        let body = self.decoded_body()?;
        let names = match self.path.as_str() {
            "/api/beta/sketches" => SketchPayload::parse_from_bytes(&body)
                .map(|p| p.sketches.iter().map(|m| m.metric.to_string()).collect()),
            "/api/v2/series" => MetricPayload::parse_from_bytes(&body)
                .map(|p| p.series.iter().map(|m| m.metric.to_string()).collect()),
            _ => return Err(format!("{} {}: not a metrics path", self.method, self.path)),
        };
        names.map_err(|e| format!("{} {}: undecodable metrics: {e}", self.method, self.path))
    }
}

#[derive(Clone)]
struct Shared {
    mode: Arc<Mutex<FaultMode>>,
    path_modes: Arc<Mutex<HashMap<String, FaultMode>>>,
    /// Bodies over this size get a 413, in any mode but `Blackhole`.
    reject_over: Arc<Mutex<Option<usize>>>,
    /// Bodies sent back for a path in `Ok` mode, instead of `{}`.
    ok_bodies: Arc<Mutex<HashMap<String, String>>>,
    requests: Arc<Mutex<Vec<IntakeRequest>>>,
    connections: Arc<AtomicUsize>,
    held: Arc<Mutex<Vec<TcpStream>>>,
}

pub struct FaultIntake {
    pub addr: SocketAddr,
    shared: Shared,
    handle: JoinHandle<()>,
}

impl FaultIntake {
    /// Bind `127.0.0.1:0` and serve in `mode`.
    pub async fn start(mode: FaultMode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fault intake");
        let addr = listener.local_addr().expect("local addr");
        let shared = Shared {
            mode: Arc::new(Mutex::new(mode)),
            path_modes: Arc::new(Mutex::new(HashMap::new())),
            reject_over: Arc::new(Mutex::new(None)),
            ok_bodies: Arc::new(Mutex::new(HashMap::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            connections: Arc::new(AtomicUsize::new(0)),
            held: Arc::new(Mutex::new(Vec::new())),
        };
        let accept_shared = shared.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    // EMFILE and friends: do not spin.
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                };
                accept_shared.connections.fetch_add(1, Ordering::SeqCst);
                // A connection accepted in blackhole mode is never read, unless a path has
                // its own mode: then `handle` reads the request head to pick the mode.
                let blackhole =
                    *accept_shared.mode.lock().expect("mode lock") == FaultMode::Blackhole;
                if blackhole
                    && accept_shared
                        .path_modes
                        .lock()
                        .expect("path modes lock")
                        .is_empty()
                {
                    accept_shared.held.lock().expect("held lock").push(stream);
                    continue;
                }
                let conn_shared = accept_shared.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(conn_shared.clone(), req));
                    let _ = http1::Builder::new()
                        .keep_alive(true)
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            addr,
            shared,
            handle,
        }
    }

    /// `http://host:port`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    // --- knobs ------------------------------------------------------------

    pub fn set_mode(&self, mode: FaultMode) {
        *self.shared.mode.lock().expect("mode lock") = mode;
    }

    /// Mode for one exact path, overriding the global mode.
    pub fn set_path_mode(&self, path: &str, mode: FaultMode) {
        self.shared
            .path_modes
            .lock()
            .expect("path modes lock")
            .insert(path.to_string(), mode);
    }

    pub fn clear_path_mode(&self, path: &str) {
        self.shared
            .path_modes
            .lock()
            .expect("path modes lock")
            .remove(path);
    }

    /// Answer `413` to any body larger than `bytes`.
    pub fn set_reject_over(&self, bytes: Option<usize>) {
        *self.shared.reject_over.lock().expect("reject lock") = bytes;
    }

    /// Body returned for `path` on a `202`, instead of `{}`.
    pub fn set_ok_body(&self, path: &str, body: &str) {
        self.shared
            .ok_bodies
            .lock()
            .expect("ok bodies lock")
            .insert(path.to_string(), body.to_string());
    }

    // --- request log --------------------------------------------------------

    pub fn requests(&self) -> Vec<IntakeRequest> {
        self.shared.requests.lock().expect("requests lock").clone()
    }

    /// Requests whose path contains `needle`.
    pub fn requests_to(&self, needle: &str) -> Vec<IntakeRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.contains(needle))
            .collect()
    }

    pub fn count(&self, needle: &str) -> usize {
        self.requests_to(needle).len()
    }

    /// Accepted TCP connections, including blackholed ones.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// `METHOD path -> status [mode] size` per request, for assertion messages.
    pub fn summary(&self) -> String {
        self.requests()
            .iter()
            .map(|r| {
                format!(
                    "{} {} -> {} [{}] {}B",
                    r.method, r.path, r.status, r.mode, r.body_len
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Wait until `n` requests that match `pred` arrived by `deadline`. A
    /// request that came late counts as missed even when the wait starts after it.
    pub async fn wait_for(
        &self,
        n: usize,
        pred: impl Fn(&IntakeRequest) -> bool,
        deadline: Instant,
    ) -> bool {
        loop {
            let now = Instant::now();
            let matched = self
                .requests()
                .iter()
                .filter(|r| r.at <= deadline && pred(r))
                .count();
            if matched >= n {
                return true;
            }
            if now > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait until `n` requests to a path containing `needle` arrived by `deadline`.
    pub async fn wait_for_path(&self, needle: &str, n: usize, deadline: Instant) -> bool {
        self.wait_for(n, |r| r.path.contains(needle), deadline)
            .await
    }
}

impl Drop for FaultIntake {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn effective_mode(shared: &Shared, path: &str) -> FaultMode {
    if let Some(m) = shared.path_modes.lock().expect("path modes lock").get(path) {
        return m.clone();
    }
    shared.mode.lock().expect("mode lock").clone()
}

fn full(body: impl Into<Bytes>) -> BoxBody<Bytes, Infallible> {
    Full::new(body.into()).boxed()
}

fn status_response(status: u16, body: String) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("content-type", "text/plain")
        .body(full(body))
        .expect("response")
}

async fn handle(
    shared: Shared,
    req: Request<Incoming>,
) -> Result<Response<BoxBody<Bytes, Infallible>>, hyper::Error> {
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let headers: HashMap<String, String> = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_ascii_lowercase(),
                v.to_str().unwrap_or("<binary>").to_string(),
            )
        })
        .collect();
    let mode = effective_mode(&shared, &path);
    let mut request = IntakeRequest {
        method,
        path,
        headers,
        body_len: 0,
        body: Vec::new(),
        at: Instant::now(),
        mode: mode.name(),
        status: 0,
    };
    if mode == FaultMode::Blackhole {
        // Reached when a path has its own mode, or the mode changed on an open
        // connection: record the head, never read the body, hold the connection.
        record(&shared, request);
        match std::future::pending::<Infallible>().await {}
    }
    request.body = match req.into_body().collect().await {
        Ok(c) => c.to_bytes().to_vec(),
        Err(e) => {
            // The upload broke off, so the intake never received it: record
            // nothing and close the connection without an answer.
            eprintln!(
                "fault intake: incomplete {} {}: {e}",
                request.method, request.path
            );
            return Err(e);
        }
    };
    request.body_len = request.body.len();
    request.at = Instant::now();
    let reject_over = *shared.reject_over.lock().expect("reject lock");

    let (status, response) = if reject_over.is_some_and(|max| request.body_len > max) {
        (
            413,
            status_response(413, "fault:413 payload too large".into()),
        )
    } else {
        match &mode {
            FaultMode::Ok => {
                let body = shared
                    .ok_bodies
                    .lock()
                    .expect("ok bodies lock")
                    .get(&request.path)
                    .cloned()
                    .unwrap_or_else(|| "{}".to_string());
                (
                    202,
                    Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .header("content-type", "application/json")
                        .body(full(body))
                        .expect("response"),
                )
            }
            FaultMode::Status(code) => (*code, status_response(*code, format!("fault:{code}"))),
            FaultMode::Slow(delay) => {
                tokio::time::sleep(*delay).await;
                (202, status_response(202, "{}".into()))
            }
            FaultMode::Blackhole => unreachable!("held before the body is read"),
            FaultMode::RetryAfter { status, secs } => (
                *status,
                Response::builder()
                    .status(StatusCode::from_u16(*status).unwrap_or(StatusCode::TOO_MANY_REQUESTS))
                    .header("retry-after", secs.to_string())
                    .body(full(format!("fault:{status} retry later")))
                    .expect("response"),
            ),
            FaultMode::CloseMidBody => {
                // A `Content-Length` larger than the one chunk we send: hyper
                // ends the connection early and the client sees a truncated body.
                let frames = vec![Ok::<_, Infallible>(Frame::data(Bytes::from_static(
                    b"{\"partial\":",
                )))];
                let body = StreamBody::new(stream::iter(frames)).boxed();
                let response = Response::builder()
                    .status(StatusCode::OK)
                    .header("content-length", "4096")
                    .body(body)
                    .expect("response");
                (200, response)
            }
        }
    };
    request.status = status;
    record(&shared, request);
    Ok(response)
}

fn record(shared: &Shared, request: IntakeRequest) {
    shared.requests.lock().expect("requests lock").push(request);
}

/// A listener whose accept queue is full, so new connections never complete
/// their handshake: the kernel drops the SYN or leaves it half-open and the
/// client's connect blocks until its timeout. Needs no privileges. Linux
/// honors the backlog exactly; a kernel that does not is detected by
/// [`is_saturated`](Self::is_saturated). Build it inside a tokio runtime.
pub struct SynBlackhole {
    pub addr: SocketAddr,
    _listener: TcpListener,
    _fillers: Vec<std::net::TcpStream>,
}

impl SynBlackhole {
    pub fn start() -> Self {
        let socket = TcpSocket::new_v4().expect("socket");
        socket
            .bind("127.0.0.1:0".parse().expect("addr"))
            .expect("bind");
        let listener = socket.listen(1).expect("listen");
        let addr = listener.local_addr().expect("local addr");
        // Fill the accept queue. Once full, a connect hangs, so stop on timeout.
        let mut fillers = Vec::new();
        for _ in 0..8 {
            match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
                Ok(s) => fillers.push(s),
                Err(_) => break,
            }
        }
        Self {
            addr,
            _listener: listener,
            _fillers: fillers,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Whether the queue is really full: a fresh connect must time out.
    pub fn is_saturated(&self) -> bool {
        std::net::TcpStream::connect_timeout(&self.addr, Duration::from_millis(300)).is_err()
    }
}

/// A `host:port` nothing listens on. Connects fail at once with ECONNREFUSED,
/// which keeps trace stats (they follow `DD_SITE`) off the network.
pub const CLOSED_PORT_URL: &str = "http://127.0.0.1:1";
