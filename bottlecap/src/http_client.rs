use crate::config;
use core::time::Duration;
use std::sync::Arc;
use tracing::error;

pub fn get_client(config: Arc<config::Config>) -> reqwest::Client {
    match build_client(config) {
        Ok(client) => client,
        Err(e) => {
            error!(
                "Unable to parse proxy configuration: {}, no proxy will be used",
                e
            );
            //TODO this fallback doesn't respect the flush timeout
            reqwest::Client::new()
        }
    }
}

fn build_client(config: Arc<config::Config>) -> Result<reqwest::Client, reqwest::Error> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.flush_timeout))
        // Enable HTTP/2 for better multiplexing
        .http2_prior_knowledge()
        // Set keep-alive timeout
        .pool_idle_timeout(Some(Duration::from_secs(90)))
        // Set maximum idle connections per host
        .pool_max_idle_per_host(32)
        // Enable TCP keepalive
        .tcp_keepalive(Some(Duration::from_secs(60)));
    // This covers DD_PROXY_HTTPS and HTTPS_PROXY
    if let Some(https_uri) = &config.https_proxy {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        client.proxy(proxy).build()
    } else {
        client.build()
    }
}
