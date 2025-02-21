use crate::config;
use core::time::Duration;
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

fn build_client(config: Arc<config::Config>) -> Result<reqwest::Client, reqwest::Error> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.flush_timeout))
        .tcp_nodelay(false)
        //.http1_only()
        // Enable HTTP/2 for better multiplexing
        .http2_prior_knowledge()
        .http2_keep_alive_interval(Some(Duration::from_secs(10)))
        .http2_keep_alive_while_idle(true)
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_initial_stream_window_size(5_000_000) // magic number
        // Set keep-alive timeout
        .pool_idle_timeout(Some(Duration::from_secs(90)))
        // Set maximum idle connections per host
        .pool_max_idle_per_host(4)
        // Enable TCP keepalive
        .tcp_keepalive(Some(Duration::from_secs(120)))
        .connection_verbose(true);
    // This covers DD_PROXY_HTTPS and HTTPS_PROXY
    if let Some(https_uri) = &config.https_proxy {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        client.proxy(proxy).build()
    } else {
        client.build()
    }
}
