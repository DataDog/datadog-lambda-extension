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
    if let Some(http_uri) = &config.http_proxy {
        let proxy = reqwest::Proxy::http(http_uri.clone())?;
        reqwest::Client::builder().proxy(proxy).build()
    } else if let Some(https_uri) = &config.https_proxy {
        let proxy = reqwest::Proxy::https(https_uri.clone())?;
        reqwest::Client::builder().proxy(proxy).build()
    } else {
        Ok(reqwest::Client::new())
    }
}
