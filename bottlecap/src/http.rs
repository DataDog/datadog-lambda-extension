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
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::NamedTempFile;
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

    // ---------------------------------------------------------------------
    // TLS root-trust tests for cold-start hypothesis H1 (PR #1276). The
    // non-FIPS build trusts compiled-in webpki roots; `tls_cert_file` is the
    // supported mitigation for environments behind a private CA (e.g. a
    // TLS-intercepting DD_PROXY_HTTPS). These pin both halves of that contract
    // so the trust model can't regress silently.
    // ---------------------------------------------------------------------

    // Self-contained test PKI, generated offline with OpenSSL (ECDSA P-256,
    // valid ~100 years). The CA is a *private* root deliberately NOT in the
    // webpki/Mozilla bundle, so it exercises exactly the path this PR changes.
    // Regenerate with:
    //   openssl ecparam -name prime256v1 -genkey -noout -out ca.key
    //   openssl req -x509 -new -key ca.key -sha256 -days 36500 \
    //     -subj "/CN=Bottlecap Test CA" \
    //     -addext "basicConstraints=critical,CA:TRUE" \
    //     -addext "keyUsage=critical,keyCertSign,cRLSign" -out ca.crt
    //   openssl ecparam -name prime256v1 -genkey -noout -out leaf.key
    //   openssl req -new -key leaf.key -subj "/CN=localhost" -out leaf.csr
    //   printf 'basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost\n' > leaf.ext
    //   openssl x509 -req -in leaf.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
    //     -sha256 -days 36500 -extfile leaf.ext -out leaf.crt
    //   openssl pkcs8 -topk8 -nocrypt -in leaf.key -out leaf.pk8.pem
    const TEST_CA_CERT_PEM: &str = r"
-----BEGIN CERTIFICATE-----
MIIBnzCCAUWgAwIBAgIUf5l+dmNjL/xU10EM0qk4Iseh3nQwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRQm90dGxlY2FwIFRlc3QgQ0EwIBcNMjYwNjI1MTgzNjMwWhgP
MjEyNjA2MDExODM2MzBaMBwxGjAYBgNVBAMMEUJvdHRsZWNhcCBUZXN0IENBMFkw
EwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEK9O0WnLz1pRAD3RZzYzsCf8vEieLLwnV
D8RNYJBaEBZBJbE+4snr+0vhVm2mGtIojZTJ0bc5Z98JHpZ57LBpwqNjMGEwHQYD
VR0OBBYEFJtZHbWEv1FI0KacM/4ctJkUuMg2MB8GA1UdIwQYMBaAFJtZHbWEv1FI
0KacM/4ctJkUuMg2MA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgEGMAoG
CCqGSM49BAMCA0gAMEUCIQDFLc+zjel9LGmytihROgErrPc6WDxmziypva+k2cSN
CAIgYb1nsDag2/bwlzf4OOcYvqU9xNRdXMarkRxn4o5rPR4=
-----END CERTIFICATE-----
";
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

    /// Installs a process-default rustls `CryptoProvider` matching the build's
    /// TLS backend (mirrors `traces/http_client.rs`); idempotent across tests.
    fn ensure_crypto_provider() {
        #[cfg(feature = "fips")]
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        #[cfg(not(feature = "fips"))]
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// Spawns a local HTTPS (HTTP/1.1-over-TLS) server presenting the test leaf
    /// certificate (signed by `TEST_CA_CERT_PEM`); returns the bound port.
    async fn spawn_tls_server() -> u16 {
        ensure_crypto_provider();
        let certs = rustls_pemfile::certs(&mut BufReader::new(TEST_SERVER_CERT_PEM.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .expect("parse test server cert");
        let key = rustls_pemfile::private_key(&mut BufReader::new(TEST_SERVER_KEY_PEM.as_bytes()))
            .expect("read test server key")
            .expect("test server key present");
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("build server TLS config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tls listener");
        let port = listener.local_addr().expect("listener local_addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(sock).await else {
                        return;
                    };
                    let mut buf = vec![0u8; 8192];
                    let mut accumulated = Vec::new();
                    loop {
                        let n = match tls.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        accumulated.extend_from_slice(&buf[..n]);
                        while let Some(idx) = accumulated.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            accumulated.drain(..idx + 4);
                            if tls
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

    fn ca_temp_file() -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp CA file");
        f.write_all(TEST_CA_CERT_PEM.as_bytes())
            .expect("write temp CA file");
        f
    }

    /// `tls_cert_file` must add the private CA *on top of* the webpki roots, so a
    /// server presenting a privately-signed cert is trusted. This is the
    /// documented mitigation for private-CA / TLS-intercepting-proxy setups.
    #[tokio::test]
    async fn tls_cert_file_trusts_private_ca() {
        let port = spawn_tls_server().await;
        let ca = ca_temp_file();
        let cfg = config::Config {
            http_protocol: Some("http1".to_string()),
            flush_timeout: 5,
            tls_cert_file: Some(ca.path().to_str().expect("ca path utf8").to_string()),
            ..config::Config::default()
        };
        let client = get_client(&Arc::new(cfg));
        let resp = client
            .get(format!("https://localhost:{port}/"))
            .send()
            .await
            .expect("request should succeed when tls_cert_file trusts the private CA");
        assert_eq!(resp.status(), 200);
    }

    /// Without `tls_cert_file`, the default root set (webpki on non-FIPS builds)
    /// must reject a privately-signed cert. Pins the post-PR trust behavior:
    /// private-CA users must opt in via `tls_cert_file`.
    #[tokio::test]
    async fn private_ca_rejected_without_tls_cert_file() {
        let port = spawn_tls_server().await;
        let cfg = config::Config {
            http_protocol: Some("http1".to_string()),
            flush_timeout: 5,
            ..config::Config::default()
        };
        let client = get_client(&Arc::new(cfg));
        let result = client
            .get(format!("https://localhost:{port}/"))
            .send()
            .await;
        assert!(
            result.is_err(),
            "default roots must reject a privately-signed cert without tls_cert_file"
        );
    }

    /// `skip_ssl_validation` remains a full escape hatch: an untrusted cert is
    /// accepted regardless of the configured root set.
    #[tokio::test]
    async fn skip_ssl_validation_accepts_untrusted_cert() {
        let port = spawn_tls_server().await;
        let cfg = config::Config {
            http_protocol: Some("http1".to_string()),
            flush_timeout: 5,
            skip_ssl_validation: true,
            ..config::Config::default()
        };
        let client = get_client(&Arc::new(cfg));
        let resp = client
            .get(format!("https://localhost:{port}/"))
            .send()
            .await
            .expect("request should succeed when skip_ssl_validation is set");
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn load_custom_cert_parses_single() {
        let ca = ca_temp_file();
        let certs = load_custom_cert(ca.path().to_str().expect("path utf8"))
            .expect("should parse one certificate");
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn load_custom_cert_parses_multiple() {
        let mut f = NamedTempFile::new().expect("temp file");
        f.write_all(format!("{TEST_CA_CERT_PEM}{TEST_SERVER_CERT_PEM}").as_bytes())
            .expect("write certs");
        let certs = load_custom_cert(f.path().to_str().expect("path utf8"))
            .expect("should parse two certificates");
        assert_eq!(certs.len(), 2);
    }

    #[test]
    fn load_custom_cert_empty_file_errors() {
        let f = NamedTempFile::new().expect("temp file");
        assert!(
            load_custom_cert(f.path().to_str().expect("path utf8")).is_err(),
            "empty file must error"
        );
    }

    #[test]
    fn load_custom_cert_non_pem_errors() {
        let mut f = NamedTempFile::new().expect("temp file");
        f.write_all(b"this is not a certificate")
            .expect("write garbage");
        assert!(
            load_custom_cert(f.path().to_str().expect("path utf8")).is_err(),
            "non-PEM content must error"
        );
    }
}
