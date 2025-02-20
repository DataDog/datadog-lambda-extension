use crate::config;
use crate::http_client;
use crate::logs::aggregator::Aggregator;
use std::{
    error::Error,
    io::Write,
    time::Instant,
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
    config: Arc<config::Config>,
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
            config,
        }
    }
    pub async fn flush(&self) {
        let lock_time = Instant::now();
        let mut guard = self.aggregator.lock().expect("lock poisoned");
        println!("lock time {:?}", lock_time.elapsed());
        let mut set = JoinSet::new();

        let batch_time = Instant::now();
        let mut logs = guard.get_batch();
        println!("batch time {:?}", batch_time.elapsed());
        let spawn_time = Instant::now();
        while !logs.is_empty() {
            let api_key = self.api_key.clone();
            let site = self.fqdn_site.clone();
            let cloned_client = self.client.clone();
            let cloned_use_compression = self.config.logs_config_use_compression;
            let cloned_compression_level = self.config.logs_config_compression_level;
            set.spawn(async move {
                Self::send(
                    cloned_client,
                    api_key,
                    site,
                    logs,
                    cloned_use_compression,
                    cloned_compression_level,
                )
                .await;
            });
            logs = guard.get_batch();
        }
        println!("spawn time {:?}", spawn_time.elapsed());
        drop(guard);
        let wait_time = Instant::now();
        while let Some(res) = set.join_next().await {
            match res {
                Ok(()) => (),
                Err(e) => {
                    error!("Failed to wait for request sending {}", e);
                }
            }
        }
        println!("log POST time {:?}", wait_time.elapsed());
    }

    #[allow(clippy::unwrap_used)]
    async fn send(
        client: reqwest::Client,
        api_key: String,
        fqdn: String,
        data: Vec<u8>,
        compression_enabled: bool,
        compression_level: i32,
    ) {
        let url = format!("{fqdn}/api/v2/logs");
        if !data.is_empty() {
            let body = if compression_enabled {
                let result = (|| -> Result<Vec<u8>, Box<dyn Error>> {
                    let mut encoder = Encoder::new(Vec::new(), compression_level)
                        .map_err(|e| Box::new(e) as Box<dyn Error>)?;

                    encoder
                        .write_all(&data)
                        .map_err(|e| Box::new(e) as Box<dyn Error>)?;

                    encoder.finish().map_err(|e| Box::new(e) as Box<dyn Error>)
                })();

                if let Ok(compressed_data) = result {
                    compressed_data
                } else {
                    debug!("Failed to compress data, sending uncompressed data");
                    data
                }
            } else {
                data
            };
            let req = client
                .post(&url)
                .header("DD-API-KEY", api_key)
                .header("DD-PROTOCOL", "agent-json")
                .header("Content-Type", "application/json");
            let req = if compression_enabled {
                req.header("Content-Encoding", "zstd")
            } else {
                req
            };
            let resp: Result<reqwest::Response, reqwest::Error> = req.body(body).send().await;

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
