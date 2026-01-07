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
use tracing::{debug, error};

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
        .pool_idle_timeout(Some(Duration::from_secs(270)))
        // Enable TCP keepalive
        .tcp_keepalive(Some(Duration::from_secs(120)));

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
