use crate::config;
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
            reqwest::Client::new()
        }
    }
}

fn build_client(config: Arc<config::Config>) -> Result<reqwest::Client, reqwest::Error> {
    let client = reqwest::Client::builder();
    // This covers DD_PROXY_HTTPS and HTTPS_PROXY
    if let Some(https_uri) = &config.https_proxy {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        client.proxy(proxy).build()
    } else {
        client.build()
    }
}
