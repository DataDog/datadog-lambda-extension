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
    let client = reqwest::Client::builder().timeout(Duration::from_secs(config.flush_timeout));
    // This covers DD_PROXY_HTTPS and HTTPS_PROXY
    if let Some(https_uri) = &config.https_proxy {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        client.proxy(proxy).build()
    } else {
        client.build()
    }
}
