use crate::config;
use axum::{
    extract::{FromRequest, Request},
    http::{self, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use core::time::Duration;
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use std::sync::Arc;
use std::{collections::HashMap, error::Error, fs::File, io::BufReader};
use tracing::{debug, error, warn};

#[must_use]
pub fn get_client(config: &Arc<config::Config>) -> reqwest::Client {
    build_client(config).unwrap_or_else(|e| {
        error!(
            "Unable to parse proxy configuration: {}, no proxy will be used",
            e
        );
        //TODO this fallback doesn't respect the flush timeout
        reqwest::Client::new()
    })
}

fn build_client(config: &Arc<config::Config>) -> Result<reqwest::Client, Box<dyn Error>> {
    let mut client = create_reqwest_client_builder()?
        .timeout(Duration::from_secs(config.flush_timeout))
        // Disable connection pooling to avoid stale connections after Lambda freeze/resume.
        // The execution environment can be frozen for seconds to minutes between invocations;
        // pooled connections become stale during this time and fail on reuse, surfacing as
        // "Max retries exceeded" in the trace proxy / metrics / logs flushers. Mirrors the
        // pattern in `traces/http_client.rs` and libdatadog's `new_client_periodic`.
        .pool_max_idle_per_host(0)
        .danger_accept_invalid_certs(config.skip_ssl_validation);

    // Determine if we should use HTTP/1 or HTTP/2
    let should_use_http1 = match &config.http_protocol {
        // Explicitly set to true - use HTTP/1
        Some(val) => val == "http1",
        // Not set - use HTTP/1 if proxy is configured, otherwise HTTP/2
        None => config.proxy_https.is_some(),
    };

    // Configure HTTP/2 if we're not using HTTP/1
    if !should_use_http1 {
        client = client
            .http2_prior_knowledge()
            .http2_keep_alive_interval(Some(Duration::from_secs(10)))
            .http2_keep_alive_while_idle(true)
            .http2_keep_alive_timeout(Duration::from_secs(1000));
    }

    if config.skip_ssl_validation && config.tls_cert_file.is_some() {
        debug!(
            "HTTP | skip_ssl_validation=true overrides tls_cert_file={:?}, custom certificate will be ignored",
            config.tls_cert_file
        );
    }

    // Load custom TLS certificate if configured
    if let Some(cert_path) = &config.tls_cert_file {
        match load_custom_cert(cert_path) {
            Ok(certs) => {
                let cert_count = certs.len();
                for cert in certs {
                    client = client.add_root_certificate(cert);
                }
                debug!(
                    "HTTP | Added {} root certificate(s) from {}",
                    cert_count, cert_path
                );
            }
            Err(e) => {
                error!(
                    "Failed to load TLS certificate from {}: {}, continuing without custom cert",
                    cert_path, e
                );
            }
        }
    }

    // This covers DD_PROXY_HTTPS and HTTPS_PROXY
    if let Some(https_uri) = &config.proxy_https {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        Ok(client.proxy(proxy).build()?)
    } else {
        Ok(client.build()?)
    }
}

fn load_custom_cert(cert_path: &str) -> Result<Vec<reqwest::Certificate>, Box<dyn Error>> {
    let file = File::open(cert_path)?;
    let mut reader = BufReader::new(file);

    // Parse PEM certificates
    let certs = rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?;

    if certs.is_empty() {
        return Err("No certificates found in file".into());
    }

    // Convert all certificates found in the file
    certs
        .into_iter()
        .map(|cert| reqwest::Certificate::from_der(&cert).map_err(Into::into))
        .collect()
}

pub async fn handler_not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

pub async fn extract_request_body(
    request: Request,
) -> Result<(http::request::Parts, Bytes), Box<dyn std::error::Error>> {
    let (parts, body) = request.into_parts();
    let bytes = Bytes::from_request(Request::from_parts(parts.clone(), body), &()).await?;

    Ok((parts, bytes))
}

/// Like [`extract_request_body`], but never fails: if buffering the body
/// errors (e.g. an oversized payload exceeding `DefaultBodyLimit`), the body
/// is replaced with empty bytes so that processing can continue with headers
/// only.
pub async fn extract_request_body_or_empty(request: Request) -> (http::request::Parts, Bytes) {
    let (parts, body) = request.into_parts();
    let bytes = match Bytes::from_request(Request::from_parts(parts.clone(), body), &()).await {
        Ok(b) => b,
        Err(e) => {
            warn!("Failed to buffer request body: {e}. Processing with empty payload.");
            Bytes::new()
        }
    };
    (parts, bytes)
}

#[must_use]
pub fn headers_to_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spawns a minimal HTTP/1.1 keep-alive server that:
    /// - increments `connection_counter` on every accepted TCP connection
    /// - serves any number of requests on the same connection until the client
    ///   closes it (matching how Datadog intakes behave in production).
    ///
    /// With pooling enabled, a client making two sequential requests will reuse
    /// the same TCP connection → counter stays at 1. With pooling disabled,
    /// each request opens a fresh connection → counter reaches 2.
    async fn spawn_keepalive_server(connection_counter: Arc<AtomicUsize>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let port = listener.local_addr().expect("listener local_addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                connection_counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut accumulated = Vec::new();
                    loop {
                        let n = match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        accumulated.extend_from_slice(&buf[..n]);
                        while let Some(idx) = accumulated.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            accumulated.drain(..idx + 4);
                            if sock
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                                )
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                });
            }
        });
        port
    }

    /// Verifies that the shared reqwest client does not pool idle connections:
    /// two sequential requests to the same host must result in two distinct
    /// TCP connections at the server. This guards against regression of
    /// [issue #1092](https://github.com/DataDog/datadog-lambda-extension/issues/1092):
    /// pooled connections go stale across Lambda freeze/resume and surface as
    /// "Max retries exceeded" errors on the proxy / metrics / logs flushers.
    #[tokio::test]
    async fn shared_client_does_not_pool_connections() {
        let counter = Arc::new(AtomicUsize::new(0));
        let port = spawn_keepalive_server(counter.clone()).await;

        let cfg = config::Config {
            // Force HTTP/1 path; HTTP/2 would also work but H1 makes the pool
            // behavior easiest to reason about with a hand-rolled server.
            http_protocol: Some("http1".to_string()),
            flush_timeout: 5,
            ..config::Config::default()
        };
        let client = get_client(&Arc::new(cfg));

        let url = format!("http://127.0.0.1:{port}/");

        // First request: opens connection #1.
        let r1 = client.get(&url).send().await.expect("first request");
        assert_eq!(r1.status(), 200);
        // Drain body so reqwest can decide whether to return the conn to the pool.
        let _ = r1.bytes().await;

        // Second request: with pooling disabled, must open a fresh connection #2.
        // With pooling enabled (the v93-era regression), reqwest would reuse
        // conn #1, which after a Lambda freeze would be stale.
        let r2 = client.get(&url).send().await.expect("second request");
        assert_eq!(r2.status(), 200);
        let _ = r2.bytes().await;

        let opened = counter.load(Ordering::SeqCst);
        assert_eq!(
            opened, 2,
            "expected 2 fresh TCP connections, got {opened} (pooling not disabled?)"
        );
    }
}
