use crate::config;
use axum::{
    extract::{FromRequest, Request},
    http::{self, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use core::time::Duration;
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use std::error::Error;
use std::sync::Arc;
use tracing::error;

#[must_use]
pub fn get_client(config: Arc<config::Config>) -> reqwest::Client {
    build_client(config).unwrap_or_else(|e| {
        error!(
            "Unable to parse proxy configuration: {}, no proxy will be used",
            e
        );
        //TODO this fallback doesn't respect the flush timeout
        reqwest::Client::new()
    })
}

fn build_client(config: Arc<config::Config>) -> Result<reqwest::Client, Box<dyn Error>> {
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
        None => config.https_proxy.is_some(),
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
    if let Some(https_uri) = &config.https_proxy {
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
