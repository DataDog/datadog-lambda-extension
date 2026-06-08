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
    let connector = if skip_ssl_validation {
        ensure_crypto_provider_initialized();

        let tls_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();

        let https_connector = HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1()
            .build();

        debug!("HTTP_CLIENT | TLS certificate validation disabled (skip_ssl_validation=true)");

        libdd_common::connector::Connector::Https(https_connector)
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
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1()
            .build();

        debug!("HTTP_CLIENT | Added root certificate from {}", ca_cert_path);

        // Construct the Connector::Https variant directly
        libdd_common::connector::Connector::Https(https_connector)
    } else {
        // Use default connector
        libdd_common::connector::Connector::default()
    };

    if let Some(proxy) = proxy_https {
        let proxy =
            hyper_http_proxy::Proxy::new(hyper_http_proxy::Intercept::Https, proxy.parse()?);
        let proxy_connector = hyper_http_proxy::ProxyConnector::from_proxy(connector, proxy)?;
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
