use crate::config;
use crate::http_client;
use crate::logs::aggregator::Aggregator;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};
use tokio::task::JoinSet;
use tracing::{debug, error};
use zstd::stream::write::Encoder;

pub struct Flusher {
    api_key: String,
    fqdn_site: String,
    client: reqwest::Client,
    aggregator: Arc<Mutex<Aggregator>>,
}

#[inline]
#[must_use]
pub fn build_fqdn_logs(site: String) -> String {
    format!("https://http-intake.logs.{site}")
}

#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(
        api_key: String,
        aggregator: Arc<Mutex<Aggregator>>,
        site: String,
        config: Arc<config::Config>,
    ) -> Self {
        let client = http_client::get_client(config.clone());
        Flusher {
            api_key,
            fqdn_site: site,
            client,
            aggregator,
        }
    }
    pub async fn flush(&self) {
        let mut guard = self.aggregator.lock().expect("lock poisoned");
        let mut set = JoinSet::new();

        let mut logs = guard.get_batch();
        while !logs.is_empty() {
            let api_key = self.api_key.clone();
            let site = self.fqdn_site.clone();
            let cloned_client = self.client.clone();
            set.spawn(async move { Self::send(cloned_client, api_key, site, logs).await });
            logs = guard.get_batch();
        }
        drop(guard);
        while let Some(res) = set.join_next().await {
            match res {
                Ok(()) => (),
                Err(e) => {
                    error!("Failed to wait for request sending {}", e);
                }
            }
        }
    }

    async fn send(client: reqwest::Client, api_key: String, fqdn: String, data: Vec<u8>) {
        let url = format!("{fqdn}/api/v2/logs");
        let mut encoder = Encoder::new(Vec::new(), 0).unwrap();
        encoder.write_all(&data).unwrap();
        let body = encoder.finish().unwrap();

        if !data.is_empty() {
            let resp: Result<reqwest::Response, reqwest::Error> = client
                .post(&url)
                .header("DD-API-KEY", api_key)
                .header("DD-PROTOCOL", "agent-json")
                .header("Content-Type", "application/json")
                .header("Content-Encoding", "zstd")
                .body(body)
                .send()
                .await;

            match resp {
                Ok(resp) => {
                    if resp.status() != 202 {
                        debug!("Failed to send logs to datadog: {}", resp.status());
                    }
                }
                Err(e) => {
                    error!("Failed to send logs to datadog: {}", e);
                }
            }
        }
    }
}
