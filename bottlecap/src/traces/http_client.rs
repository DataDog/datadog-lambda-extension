// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP client for trace and stats flushers.
//!
//! This module provides the HTTP client type required by `libdd_trace_utils`
//! for sending traces and stats to Datadog intake endpoints.

use http_body_util::BodyExt;
use hyper_http_proxy;
use hyper_rustls::HttpsConnectorBuilder;
use libdd_capabilities::{
    MaybeSend,
    http::{HttpClientCapability, HttpError},
    sleep::SleepCapability,
};
use libdd_common::http_common::{self, Body, GenericHttpClient};
use rustls::RootCertStore;
use rustls_pki_types::CertificateDer;
use std::error::Error;
use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio_rustls::TlsConnector;
use tracing::debug;

type InnerClient =
    GenericHttpClient<hyper_http_proxy::ProxyConnector<libdd_common::connector::Connector>>;

/// HTTP client used by trace and stats flushers.
///
/// Wraps a hyper client preconfigured with optional proxy and TLS settings, and
/// implements [`HttpClientCapability`] and [`SleepCapability`] so it can be
/// passed to `libdd_trace_utils` senders such as `SendData::send`.
#[derive(Clone, Debug)]
pub struct HttpClient {
    inner: InnerClient,
}

impl HttpClient {
    fn new(inner: InnerClient) -> Self {
        Self { inner }
    }
}

impl HttpClientCapability for HttpClient {
    #[allow(clippy::expect_used)]
    fn new_client() -> Self {
        // Required by `HttpClientCapability` but never invoked on bottlecap's code
        // paths — production builds the client via
        // `create_client(proxy, tls_cert, skip_ssl)`. Routing this fallback
        // through the same constructor keeps the failure mode and error
        // surface consistent with the rest of the module.
        create_client(None, None, false)
            .expect("building default proxy connector with default TLS should not fail")
    }

    #[allow(clippy::expect_used)]
    fn new_periodic() -> Self {
        // `create_client` always disables connection pooling (see its comment above), so
        // this is equivalent to `new_client()`.
        Self::new_client()
    }

    fn request(
        &self,
        req: http::Request<bytes::Bytes>,
    ) -> impl Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend {
        let client = self.inner.clone();
        async move {
            let hyper_req = req.map(Body::from_bytes);
            let response = client
                .request(hyper_req)
                .await
                .map_err(|e| HttpError::Network(e.into()))?;
            let (parts, body) = response.into_parts();
            let collected = body
                .collect()
                .await
                .map_err(|e| HttpError::ResponseBody(e.into()))?
                .to_bytes();
            Ok(http::Response::from_parts(parts, collected))
        }
    }
}

impl SleepCapability for HttpClient {
    fn new() -> Self {
        // Required by `SleepCapability` but never invoked on bottlecap's code
        // paths — production builds the client via
        // `create_client(proxy, tls_cert, skip_ssl)`. Mirror `new_client()` so
        // the failure mode stays consistent with the rest of the module.
        <Self as HttpClientCapability>::new_client()
    }

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + MaybeSend {
        tokio::time::sleep(duration)
    }
}

/// Initialize the crypto provider needed for setting custom root certificates.
fn ensure_crypto_provider_initialized() {
    static INIT_CRYPTO_PROVIDER: LazyLock<()> = LazyLock::new(|| {
        // FIPS builds use the FIPS-validated aws-lc-rs provider, non-FIPS
        // builds use ring (the libdatadog default). Pick based on the bottlecap
        // `fips` feature so we never pull both crypto backends into the binary.
        #[cfg(all(unix, feature = "fips"))]
        let provider = rustls::crypto::aws_lc_rs::default_provider();
        #[cfg(all(unix, not(feature = "fips")))]
        let provider = rustls::crypto::ring::default_provider();

        #[cfg(unix)]
        if let Err(_already_installed) = provider.install_default() {
            debug!(
                "HTTP_CLIENT | Default CryptoProvider already installed, using existing provider"
            );
        }
    });

    let () = &*INIT_CRYPTO_PROVIDER;
}

/// A certificate verifier that accepts all server certificates without validation.
///
/// This is used when `DD_SKIP_SSL_VALIDATION` is set to `true`.
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        // Safe to unwrap_or_default: the provider is always initialized via
        // ensure_crypto_provider_initialized() before NoVerifier is used.
        rustls::crypto::CryptoProvider::get_default()
            .map(|p| p.signature_verification_algorithms.supported_schemes())
            .unwrap_or_default()
    }
}

/// Creates a new HTTP client with the given configuration.
///
/// This client is compatible with `libdd_trace_utils` and supports:
/// - HTTPS proxy configuration
/// - Custom TLS root certificates
/// - Skipping TLS certificate validation
///
/// # Arguments
///
/// * `proxy_https` - Optional HTTPS proxy URL
/// * `tls_cert_file` - Optional path to a PEM file containing root certificates
/// * `skip_ssl_validation` - If true, skip TLS certificate validation
///
/// # Errors
///
/// Returns an error if:
/// - The proxy URL cannot be parsed
/// - The TLS certificate file cannot be read or parsed
pub fn create_client(
    proxy_https: Option<&String>,
    tls_cert_file: Option<&String>,
    skip_ssl_validation: bool,
) -> Result<HttpClient, Box<dyn Error>> {
    if skip_ssl_validation && tls_cert_file.is_some() {
        debug!(
            "HTTP_CLIENT | skip_ssl_validation=true overrides tls_cert_file={:?}, custom certificate will be ignored",
            tls_cert_file
        );
    }

    // Create the base connector with optional custom TLS config
    let (connector, tunnel_tls) = if skip_ssl_validation {
        ensure_crypto_provider_initialized();

        let tls_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();

        let https_connector = HttpsConnectorBuilder::new()
            .with_tls_config(tls_config.clone())
            .https_or_http()
            .enable_http1()
            .build();

        debug!("HTTP_CLIENT | TLS certificate validation disabled (skip_ssl_validation=true)");

        (
            libdd_common::connector::Connector::Https(https_connector),
            Some(TlsConnector::from(Arc::new(tls_config))),
        )
    } else if let Some(ca_cert_path) = tls_cert_file {
        // Ensure crypto provider is initialized before creating TLS config
        ensure_crypto_provider_initialized();

        // Load the custom certificate
        let cert_file = File::open(ca_cert_path)?;
        let mut reader = BufReader::new(cert_file);
        let certs: Vec<CertificateDer> =
            rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?;

        // Create a root certificate store and add custom certs
        let mut root_store = RootCertStore::empty();
        for cert in certs {
            root_store.add(cert)?;
        }

        // Build the TLS config with custom root certificates
        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        // Build the HTTPS connector with custom config
        let https_connector = HttpsConnectorBuilder::new()
            .with_tls_config(tls_config.clone())
            .https_or_http()
            .enable_http1()
            .build();

        debug!("HTTP_CLIENT | Added root certificate from {}", ca_cert_path);

        // Construct the Connector::Https variant directly
        (
            libdd_common::connector::Connector::Https(https_connector),
            Some(TlsConnector::from(Arc::new(tls_config))),
        )
    } else {
        // Use default connector
        (libdd_common::connector::Connector::default(), None)
    };

    if let Some(proxy) = proxy_https {
        let proxy =
            hyper_http_proxy::Proxy::new(hyper_http_proxy::Intercept::Https, proxy.parse()?);
        let mut proxy_connector = hyper_http_proxy::ProxyConnector::from_proxy(connector, proxy)?;
        // from_proxy() builds its own tunnel TLS connector with public webpki roots, so a CA
        // from DD_TLS_CERT_FILE (or DD_SKIP_SSL_VALIDATION) never applied to the handshake
        // with the destination. Replace it with the connector built from the same policy.
        if let Some(tls) = tunnel_tls {
            proxy_connector.set_tls(Some(tls));
        }
        // Disable connection pooling to avoid stale connections after Lambda freeze/resume.
        // In Lambda, the execution environment can be frozen for seconds to minutes between
        // invocations. Pooled connections become stale during this time, causing failures
        // when reused. Setting pool_max_idle_per_host(0) ensures each request gets a fresh
        // connection, matching the pattern used in libdatadog's new_client_periodic().
        let client = http_common::client_builder()
            .pool_max_idle_per_host(0)
            .build(proxy_connector);
        debug!(
            "HTTP_CLIENT | Proxy connector created with proxy: {:?}",
            proxy_https
        );
        Ok(HttpClient::new(client))
    } else {
        let proxy_connector = hyper_http_proxy::ProxyConnector::new(connector)?;
        // Disable connection pooling to avoid stale connections after Lambda freeze/resume.
        // See comment above for detailed explanation.
        Ok(HttpClient::new(
            http_common::client_builder()
                .pool_max_idle_per_host(0)
                .build(proxy_connector),
        ))
    }
}

#[cfg(test)]
mod tests {
    //! SLES-3105: `DD_TLS_CERT_FILE` must be honoured for the TLS handshake that
    //! happens *inside* the CONNECT tunnel, not just for the connection to the proxy.
    //!
    //! Topology under test (all on loopback):
    //!   client --HTTP--> CONNECT proxy --TCP--> TLS server (leaf signed by a private CA)
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::NamedTempFile;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

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

    /// A CA that has nothing to do with the server certificate (negative control).
    const UNRELATED_CA_PEM: &str = r"
-----BEGIN CERTIFICATE-----
MIIBfjCCASSgAwIBAgIUYP+TuKcxEIhJmpDhImnx0kx1VGMwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRVW5yZWxhdGVkIFRlc3QgQ0EwIBcNMjYxMTE2MTIwMDQwWhgP
MjEyNjEwMjMxMjAwNDBaMBwxGjAYBgNVBAMMEVVucmVsYXRlZCBUZXN0IENBMFkw
EwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEqqQSp7ePJKl/mYLhafkULk5tsx8N/eAG
D4AISM6bDMAMTYNy+b5D/o8OFi6Er9jR3SzlPP2eEpIrgw+llkFoVKNCMEAwHQYD
VR0OBBYEFM13NV0lLVfhl8ucmEirnnYgOl1HMA8GA1UdEwEB/wQFMAMBAf8wDgYD
VR0PAQH/BAQDAgEGMAoGCCqGSM49BAMCA0gAMEUCIQDpxEu50JhvSlIxMzDWboFF
NTK5yjb/KSEqOWs7Rs8B5QIgCTMvqLDKf5X3dZPGAAlYl/zmnv9DtU7Q1qT5FoK+
QUI=
-----END CERTIFICATE-----
";

    fn pem_file(pem: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("create temp pem file");
        f.write_all(pem.as_bytes()).expect("write temp pem file");
        f
    }

    fn path_string(f: &NamedTempFile) -> String {
        f.path().to_str().expect("utf8 path").to_string()
    }

    /// HTTPS server presenting the private-CA-signed leaf. `requests` counts HTTP
    /// requests that actually reached the server (i.e. handshake succeeded).
    async fn spawn_tls_server(requests: Arc<AtomicUsize>) -> u16 {
        ensure_crypto_provider_initialized();
        let certs = rustls_pemfile::certs(&mut BufReader::new(TEST_SERVER_CERT_PEM.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .expect("parse server cert");
        let key = rustls_pemfile::private_key(&mut BufReader::new(TEST_SERVER_KEY_PEM.as_bytes()))
            .expect("read server key")
            .expect("server key present");
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("server tls config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tls");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let requests = requests.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(sock).await else {
                        return;
                    };
                    let mut buf = vec![0u8; 8192];
                    let mut acc = Vec::new();
                    loop {
                        let n = match tls.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => n,
                        };
                        acc.extend_from_slice(&buf[..n]);
                        if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                            requests.fetch_add(1, Ordering::SeqCst);
                            let _ = tls
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                                .await;
                            let _ = tls.shutdown().await;
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    /// Minimal TLS-*passthrough* CONNECT proxy: it never terminates TLS, so the
    /// destination certificate is verified by the client inside the tunnel -- exactly
    /// the handshake that was using the wrong roots. `connects` counts CONNECTs seen.
    async fn spawn_connect_proxy(connects: Arc<AtomicUsize>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                let connects = connects.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match client.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&head).to_string();
                    let first = head.lines().next().unwrap_or_default();
                    let mut parts = first.split_whitespace();
                    if parts.next() != Some("CONNECT") {
                        return;
                    }
                    connects.fetch_add(1, Ordering::SeqCst);
                    let target_port: u16 = parts
                        .next()
                        .and_then(|t| t.rsplit(':').next())
                        .and_then(|p| p.parse().ok())
                        .expect("CONNECT target port");
                    let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", target_port)).await
                    else {
                        return;
                    };
                    if client
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        port
    }

    async fn get(
        client: &HttpClient,
        port: u16,
    ) -> Result<http::Response<bytes::Bytes>, HttpError> {
        let req = http::Request::builder()
            .method("GET")
            .uri(format!("https://localhost:{port}/"))
            .body(bytes::Bytes::new())
            .expect("request");
        client.request(req).await
    }

    fn proxy_url(port: u16) -> String {
        format!("http://127.0.0.1:{port}")
    }

    /// THE BUG. Private CA supplied via `DD_TLS_CERT_FILE`, destination reached through a
    /// CONNECT proxy: the tunnel handshake must trust that CA.
    #[tokio::test]
    async fn proxy_tunnel_trusts_tls_cert_file_ca() {
        let (connects, hits) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let server = spawn_tls_server(hits.clone()).await;
        let proxy = spawn_connect_proxy(connects.clone()).await;
        let ca = pem_file(TEST_CA_CERT_PEM);

        let client =
            create_client(Some(&proxy_url(proxy)), Some(&path_string(&ca)), false).expect("client");
        let resp = get(&client, server)
            .await
            .expect("request through proxy should succeed");

        assert_eq!(resp.status(), 200);
        assert_eq!(resp.body().as_ref(), b"ok");
        assert_eq!(
            connects.load(Ordering::SeqCst),
            1,
            "request must go via the proxy"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    /// Verification must stay ON: an unrelated CA must NOT make the tunnel trust the server.
    #[tokio::test]
    async fn proxy_tunnel_rejects_unrelated_ca() {
        let (connects, hits) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let server = spawn_tls_server(hits.clone()).await;
        let proxy = spawn_connect_proxy(connects.clone()).await;
        let ca = pem_file(UNRELATED_CA_PEM);

        let client =
            create_client(Some(&proxy_url(proxy)), Some(&path_string(&ca)), false).expect("client");
        assert!(
            get(&client, server).await.is_err(),
            "unrelated CA must be rejected"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "no HTTP request may reach the server"
        );
    }

    /// Without a custom CA the default (public) roots apply and a private-CA server is rejected.
    #[tokio::test]
    async fn proxy_tunnel_rejects_private_ca_without_tls_cert_file() {
        let (connects, hits) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let server = spawn_tls_server(hits.clone()).await;
        let proxy = spawn_connect_proxy(connects.clone()).await;

        let client = create_client(Some(&proxy_url(proxy)), None, false).expect("client");
        assert!(get(&client, server).await.is_err());
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    /// Same bug, other knob: `DD_SKIP_SSL_VALIDATION=true` must also apply inside the tunnel.
    #[tokio::test]
    async fn proxy_tunnel_honours_skip_ssl_validation() {
        let (connects, hits) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let server = spawn_tls_server(hits.clone()).await;
        let proxy = spawn_connect_proxy(connects.clone()).await;

        let client = create_client(Some(&proxy_url(proxy)), None, true).expect("client");
        let resp = get(&client, server)
            .await
            .expect("skip_ssl_validation must work via proxy");
        assert_eq!(resp.status(), 200);
        assert_eq!(connects.load(Ordering::SeqCst), 1);
    }

    /// Control (the customer's "direct HTTPS + trusted CA" row): already worked, must keep working.
    #[tokio::test]
    async fn direct_connection_trusts_tls_cert_file_ca() {
        let hits = Arc::new(AtomicUsize::new(0));
        let server = spawn_tls_server(hits.clone()).await;
        let ca = pem_file(TEST_CA_CERT_PEM);

        let client = create_client(None, Some(&path_string(&ca)), false).expect("client");
        let resp = get(&client, server)
            .await
            .expect("direct request should succeed");
        assert_eq!(resp.status(), 200);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
