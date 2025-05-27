use crate::config;
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
    #[cfg(not(feature = "integration_test"))]
    let client = create_reqwest_client_builder()?
        .timeout(Duration::from_secs(config.flush_timeout))
        // Enable HTTP/2 for better multiplexing
        .http2_prior_knowledge()
        .http2_keep_alive_interval(Some(Duration::from_secs(10)))
        .http2_keep_alive_while_idle(true)
        .http2_keep_alive_timeout(Duration::from_secs(1000))
        // Set keep-alive timeout
        .pool_idle_timeout(Some(Duration::from_secs(270)))
        // Enable TCP keepalive
        .tcp_keepalive(Some(Duration::from_secs(120)));

    #[cfg(feature = "integration_test")]
    let client = create_reqwest_client_builder();

    // This covers DD_PROXY_HTTPS and HTTPS_PROXY
    if let Some(https_uri) = &config.https_proxy {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        Ok(client.proxy(proxy).build()?)
    } else {
        Ok(client.build()?)
    }
}
