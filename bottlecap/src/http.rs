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
use std::{collections::HashMap, error::Error};
use std::fs::File;
use std::io::Read;
use tracing::{debug, error};

#[must_use]
pub fn get_client(config: &Arc<config::Config>) -> reqwest::Client {
    build_client(config).unwrap_or_else(|e: Box<dyn Error + 'static>| {
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

    if let Some(ca_cert_path) = &config.ssl_ca_cert {
        let mut buf = Vec::new();   
        let mut cert_file = File::open(ca_cert_path)?;
        cert_file.read_to_end(&mut buf)?;
        let cert = reqwest::Certificate::from_pem(&buf)?;
        client = client.add_root_certificate(cert);
        debug!("HTTP | Added root certificate from {}", ca_cert_path);
    }

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

    // This covers DD_PROXY_HTTPS and HTTPS_PROXY
    if let Some(https_uri) = &config.proxy_https {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        Ok(client.proxy(proxy).build()?)
    } else {
        Ok(client.build()?)
    }
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
