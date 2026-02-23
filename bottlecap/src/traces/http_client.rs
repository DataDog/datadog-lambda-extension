// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP client for trace and stats flushers.
//!
//! This module provides the HTTP client type required by `libdd_trace_utils`
//! for sending traces and stats to Datadog intake endpoints.

use hyper_http_proxy;
use hyper_rustls::HttpsConnectorBuilder;
use libdd_common::{GenericHttpClient, http_common};
use rustls::RootCertStore;
use rustls_pki_types::CertificateDer;
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::sync::LazyLock;
use tracing::debug;

/// Type alias for the HTTP client used by trace and stats flushers.
///
/// This is the client type expected by `libdd_trace_utils::SendData::send()`.
pub type HttpClient =
    GenericHttpClient<hyper_http_proxy::ProxyConnector<libdd_common::connector::Connector>>;

/// Initialize the crypto provider needed for setting custom root certificates.
fn ensure_crypto_provider_initialized() {
    static INIT_CRYPTO_PROVIDER: LazyLock<()> = LazyLock::new(|| {
        #[cfg(unix)]
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("Failed to install default CryptoProvider");
    });

    let () = &*INIT_CRYPTO_PROVIDER;
}

/// Creates a new HTTP client with the given configuration.
///
/// This client is compatible with `libdd_trace_utils` and supports:
/// - HTTPS proxy configuration
/// - Custom TLS root certificates
///
/// # Arguments
///
/// * `proxy_https` - Optional HTTPS proxy URL
/// * `tls_cert_file` - Optional path to a PEM file containing root certificates
///
/// # Errors
///
/// Returns an error if:
/// - The proxy URL cannot be parsed
/// - The TLS certificate file cannot be read or parsed
pub fn create_client(
    proxy_https: Option<&String>,
    tls_cert_file: Option<&String>,
) -> Result<HttpClient, Box<dyn Error>> {
    // Create the base connector with optional custom TLS config
    let connector = if let Some(ca_cert_path) = tls_cert_file {
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
        let client = http_common::client_builder().build(proxy_connector);
        debug!(
            "HTTP_CLIENT | Proxy connector created with proxy: {:?}",
            proxy_https
        );
        Ok(client)
    } else {
        let proxy_connector = hyper_http_proxy::ProxyConnector::new(connector)?;
        Ok(http_common::client_builder().build(proxy_connector))
    }
}
