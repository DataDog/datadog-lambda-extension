// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Fake logs intake for the logs contract tests and the logs benchmark.
//!
//! Accepts requests on any path over plain HTTP/1.1 or HTTPS (HTTP/1.1 only — clients must
//! not use HTTP/2 prior knowledge; set bottlecap's `http_protocol: "http1"`). HTTPS uses a
//! self-signed test certificate, so clients must skip certificate validation (bottlecap:
//! `skip_ssl_validation: true`). Every request is recorded (method, path, headers, body);
//! responses use scripted status codes, falling back to `202 Accepted`. Requests can also be
//! scripted to hang (never answered) so clients time out.

#![allow(dead_code)] // each test binary uses a different subset

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::io::BufReader;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

// Same self-signed leaf certificate as the TLS tests in `src/http.rs`.
const TEST_SERVER_CERT_PEM: &str = r"
-----BEGIN CERTIFICATE-----
MIIBvTCCAWSgAwIBAgIUOkYng/RZxQcYlMY5I+RAXzR7dSkwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRQm90dGxlY2FwIFRlc3QgQ0EwIBcNMjYwNjI1MTgzNjMwWhgP
MjEyNjA2MDExODM2MzBaMBQxEjAQBgNVBAMMCWxvY2FsaG9zdDBZMBMGByqGSM49
AgEGCCqGSM49AwEHA0IABOuKun0roY5MAEOgA3NNebQa9l56HVsNFwGJ4a5chM2T
s+vToAoyflPMZfxuS6PUv2WHrahmUH5WZKr0XaT/RIijgYkwgYYwCQYDVR0TBAIw
ADAOBgNVHQ8BAf8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwEwFAYDVR0RBA0w
C4IJbG9jYWxob3N0MB0GA1UdDgQWBBTcqKduDXHEOeUXAL6fqwAYl50VVTAfBgNV
HSMEGDAWgBSbWR21hL9RSNCmnDP+HLSZFLjINjAKBggqhkjOPQQDAgNHADBEAiAT
nwOMjOwnQoeLvZgHhNqrzlVGNqnT4FPudJTgTKrAAAIgVMw8wqk70JXLm4pEdN8/
oMUo5j7nV6S8Hd02b5hW5pM=
-----END CERTIFICATE-----
";
const TEST_SERVER_KEY_PEM: &str = r"
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgkflx4lEcI8Fafj8E
EgBejgnIKCAkaQXo+p0h3a6IsQOhRANCAATrirp9K6GOTABDoANzTXm0GvZeeh1b
DRcBieGuXITNk7Pr06AKMn5TzGX8bkuj1L9lh62oZlB+VmSq9F2k/0SI
-----END PRIVATE KEY-----
";

/// One request received by the fake intake.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// Header names are lowercase; if a header repeats, the last value wins.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Body parsed as JSON, decompressing first when `Content-Encoding: zstd` is set.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        let raw = if self.header("content-encoding") == Some("zstd") {
            zstd::decode_all(self.body.as_slice()).expect("fake_logs_intake: invalid zstd body")
        } else {
            self.body.clone()
        };
        serde_json::from_slice(&raw).expect("fake_logs_intake: body is not JSON")
    }
}

#[derive(Default)]
struct State {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    requests: Vec<RecordedRequest>,
    replies: VecDeque<Reply>,
}

/// How the intake answers one request.
enum Reply {
    Status(u16),
    /// Record the request, then never answer.
    Hang,
}

pub struct FakeLogsIntake {
    base_url: String,
    port: u16,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl FakeLogsIntake {
    /// Plain HTTP on `127.0.0.1`, OS-assigned port.
    pub async fn start_http() -> Self {
        Self::start(None).await
    }

    /// HTTPS on `127.0.0.1`, OS-assigned port, self-signed certificate.
    pub async fn start_https() -> Self {
        Self::start(Some(tls_acceptor())).await
    }

    async fn start(acceptor: Option<tokio_rustls::TlsAcceptor>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake_logs_intake: bind");
        let port = listener
            .local_addr()
            .expect("fake_logs_intake: local_addr")
            .port();
        let scheme = if acceptor.is_some() { "https" } else { "http" };
        let state = Arc::new(State::default());
        let server_state = Arc::clone(&state);

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let state = Arc::clone(&server_state);
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(Arc::clone(&state), req));
                    match acceptor {
                        Some(acceptor) => {
                            let Ok(tls) = acceptor.accept(stream).await else {
                                return;
                            };
                            let _ = http1::Builder::new()
                                .serve_connection(TokioIo::new(tls), service)
                                .await;
                        }
                        None => {
                            let _ = http1::Builder::new()
                                .serve_connection(TokioIo::new(stream), service)
                                .await;
                        }
                    }
                });
            }
        });

        Self {
            base_url: format!("{scheme}://127.0.0.1:{port}"),
            port,
            state,
            task,
        }
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Replies to the next requests with `statuses`, in order; later requests get 202.
    pub fn script_statuses(&self, statuses: &[u16]) {
        self.state
            .inner
            .lock()
            .expect("fake_logs_intake: lock")
            .replies
            .extend(statuses.iter().copied().map(Reply::Status));
    }

    /// Records the next `count` requests but never answers them, so clients time out.
    pub fn script_hangs(&self, count: usize) {
        self.state
            .inner
            .lock()
            .expect("fake_logs_intake: lock")
            .replies
            .extend(std::iter::repeat_with(|| Reply::Hang).take(count));
    }

    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state
            .inner
            .lock()
            .expect("fake_logs_intake: lock")
            .requests
            .clone()
    }

    /// Returns the recorded requests and clears the log.
    pub fn take_requests(&self) -> Vec<RecordedRequest> {
        std::mem::take(
            &mut self
                .state
                .inner
                .lock()
                .expect("fake_logs_intake: lock")
                .requests,
        )
    }
}

impl Drop for FakeLogsIntake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(
    state: Arc<State>,
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let headers = req
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default();
    let reply = {
        let mut inner = state.inner.lock().expect("fake_logs_intake: lock");
        let reply = inner.replies.pop_front().unwrap_or(Reply::Status(202));
        inner.requests.push(RecordedRequest {
            method,
            path,
            headers,
            body,
        });
        reply
    };
    let status = match reply {
        Reply::Status(status) => status,
        Reply::Hang => std::future::pending().await,
    };
    Ok(Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .expect("fake_logs_intake: response"))
}

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    // Mirrors `ensure_crypto_provider` in `src/http.rs`; idempotent.
    #[cfg(feature = "fips")]
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    #[cfg(not(feature = "fips"))]
    let _ = rustls::crypto::ring::default_provider().install_default();

    let certs = rustls_pemfile::certs(&mut BufReader::new(TEST_SERVER_CERT_PEM.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .expect("fake_logs_intake: parse cert");
    let key = rustls_pemfile::private_key(&mut BufReader::new(TEST_SERVER_KEY_PEM.as_bytes()))
        .expect("fake_logs_intake: read key")
        .expect("fake_logs_intake: key present");
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("fake_logs_intake: tls config");
    tokio_rustls::TlsAcceptor::from(Arc::new(config))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // plain reqwest client against a local server
mod self_tests {
    use super::*;

    #[tokio::test]
    async fn records_requests_and_replays_scripted_statuses() {
        let intake = FakeLogsIntake::start_http().await;
        intake.script_statuses(&[500]);
        let client = reqwest::Client::builder().build().expect("client");

        let first = client
            .post(format!("{}/api/v2/logs", intake.base_url()))
            .header("DD-API-KEY", "k")
            .body("[1]")
            .send()
            .await
            .expect("send");
        let second = client
            .post(format!("{}/other", intake.base_url()))
            .body("[2]")
            .send()
            .await
            .expect("send");

        assert_eq!(first.status().as_u16(), 500);
        assert_eq!(second.status().as_u16(), 202);
        let requests = intake.take_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].path, "/api/v2/logs");
        assert_eq!(requests[0].header("DD-API-KEY"), Some("k"));
        assert_eq!(requests[0].json(), serde_json::json!([1]));
        assert_eq!(requests[1].path, "/other");
        assert!(intake.requests().is_empty(), "take_requests clears the log");
    }

    #[tokio::test]
    async fn decodes_zstd_bodies() {
        let intake = FakeLogsIntake::start_http().await;
        let client = reqwest::Client::builder().build().expect("client");
        let body = zstd::encode_all(&b"[{\"a\":1}]"[..], 3).expect("zstd");
        client
            .post(format!("{}/api/v2/logs", intake.base_url()))
            .header("Content-Encoding", "zstd")
            .body(body)
            .send()
            .await
            .expect("send");
        assert_eq!(intake.requests()[0].json(), serde_json::json!([{"a": 1}]));
    }

    #[tokio::test]
    async fn serves_https_with_the_test_certificate() {
        let intake = FakeLogsIntake::start_https().await;
        assert!(intake.base_url().starts_with("https://127.0.0.1:"));
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("client");
        let resp = client
            .post(format!("{}/api/v2/logs", intake.base_url()))
            .body("[]")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status().as_u16(), 202);
        assert_eq!(intake.requests().len(), 1);
    }

    #[tokio::test]
    async fn hung_request_times_out_and_is_recorded() {
        let intake = FakeLogsIntake::start_http().await;
        intake.script_hangs(1);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .expect("client");
        let result = client
            .post(format!("{}/api/v2/logs", intake.base_url()))
            .body("[]")
            .send()
            .await;
        assert!(
            result.expect_err("a hung request must fail").is_timeout(),
            "the client times out"
        );
        let requests = intake.requests();
        assert_eq!(requests.len(), 1, "the hung request is still recorded");
        assert_eq!(requests[0].method, "POST");
    }
}
