// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! A Datadog intake that misbehaves on demand.
//!
//! Plain HTTP/1.1 on a random local port (set `DD_HTTP_PROTOCOL=http1` on the
//! extension). Every request is recorded with its path, size, headers and
//! the mode that answered it. The fault mode can change at runtime, and one
//! path can get its own mode (for example `/credentials` for the secret-fetch
//! tests).
//!
//! Mode names, shared with the Lambda simulator: `ok`, `status:<code>`,
//! `slow:<ms>`, `blackhole`, `retry_after:<code>:<secs>`, `close_mid_body`,
//! and `syn_blackhole` for [`SynBlackhole`]. These modes belong in the shared
//! `datadog-mock-intake` crate once bottlecap consumes it; see `tests/HARNESS.md`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
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

    /// Body, zstd- or gzip-decoded when `Content-Encoding` says so.
    pub fn decoded_body(&self) -> Vec<u8> {
        match self.header("content-encoding") {
            Some("zstd") => zstd::decode_all(self.body.as_slice()).unwrap_or_default(),
            Some("gzip") => {
                use std::io::Read;
                let mut out = Vec::new();
                let _ = flate2::read::GzDecoder::new(self.body.as_slice()).read_to_end(&mut out);
                out
            }
            _ => self.body.clone(),
        }
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.decoded_body()).into_owned()
    }
}

#[derive(Clone)]
struct Shared {
    mode: Arc<Mutex<FaultMode>>,
    path_modes: Arc<Mutex<HashMap<String, FaultMode>>>,
    /// Bodies over this size get a 413, whatever the mode.
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
                // its own mode: then `handle` reads the request to pick the mode.
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

    /// Wait until `n` requests match `pred`. Returns whether it happened within `budget`.
    pub async fn wait_for(
        &self,
        n: usize,
        pred: impl Fn(&IntakeRequest) -> bool,
        budget: Duration,
    ) -> bool {
        let start = std::time::Instant::now();
        loop {
            if self.requests().iter().filter(|r| pred(r)).count() >= n {
                return true;
            }
            if start.elapsed() > budget {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait until `n` requests to a path containing `needle` arrived.
    pub async fn wait_for_path(&self, needle: &str, n: usize, budget: Duration) -> bool {
        self.wait_for(n, |r| r.path.contains(needle), budget).await
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
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
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
    let body = req
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes().to_vec())
        .unwrap_or_default();
    let mode = effective_mode(&shared, &path);
    let reject_over = *shared.reject_over.lock().expect("reject lock");

    let (status, response) = if reject_over.is_some_and(|max| body.len() > max) {
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
                    .get(&path)
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
            FaultMode::Blackhole => {
                // A request on a connection opened before the mode changed:
                // record it, then hold it open for the life of the connection.
                record(&shared, &method, &path, headers, body, &mode, 0);
                std::future::pending::<()>().await;
                unreachable!()
            }
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
    record(&shared, &method, &path, headers, body, &mode, status);
    Ok(response)
}

fn record(
    shared: &Shared,
    method: &str,
    path: &str,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    mode: &FaultMode,
    status: u16,
) {
    shared
        .requests
        .lock()
        .expect("requests lock")
        .push(IntakeRequest {
            method: method.to_string(),
            path: path.to_string(),
            headers,
            body_len: body.len(),
            body,
            mode: mode.name(),
            status,
        });
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
