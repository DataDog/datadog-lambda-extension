use crate::config;
use crate::logs::aggregator::Aggregator;
use hyper::{client::ResponseFuture, Body, Client, HeaderMap, Method, Request};
use std::time::{Duration, Instant};
use std::{
    error::Error,
    io::Write,
    sync::{Arc, Mutex},
};
use tokio::task::JoinSet;
use tracing::{debug, error};
use zstd::stream::write::Encoder;

#[derive(Debug, Clone)]
pub struct Flusher {
    fqdn_site: String,
    client: hyper::Client<hyper::client::HttpConnector>,
    aggregator: Arc<Mutex<Aggregator>>,
    config: Arc<config::Config>,
    headers: HeaderMap,
}

#[inline]
#[must_use]
pub fn build_fqdn_logs(site: String) -> String {
    format!("https://http-intake.logs.{site}")
}

impl Flusher {
    pub fn new(
        api_key: String,
        aggregator: Arc<Mutex<Aggregator>>,
        site: String,
        config: Arc<config::Config>,
    ) -> Self {
        // let client = http_client::get_client(config.clone());
        let client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            .http2_only(true)
            .http2_keep_alive_while_idle(true)
            .build_http();
        let mut headers = HeaderMap::new();
        headers.insert(
            "DD-API-KEY",
            api_key.clone().parse().expect("failed to parse header"),
        );
        headers.insert(
            "DD-PROTOCOL",
            "agent-json".parse().expect("failed to parse header"),
        );
        headers.insert(
            "Content-Type",
            "application/json".parse().expect("failed to parse header"),
        );

        if config.logs_config_use_compression {
            headers.insert(
                "Content-Encoding",
                "zstd".parse().expect("failed to parse header"),
            );
        }

        Flusher {
            fqdn_site: site,
            client,
            aggregator,
            config,
            headers,
        }
    }
    pub async fn flush(&self) {
        let logs_batches = {
            let mut guard = self.aggregator.lock().expect("lock poisoned");
            let mut batches = Vec::new();
            let mut current_batch = guard.get_batch();
            while !current_batch.is_empty() {
                batches.push(current_batch);
                current_batch = guard.get_batch();
            }

            batches
        };

        println!("Flushing {} logs batches", logs_batches.len());

        let mut set = JoinSet::new();
        for batch in &logs_batches {
            if batch.is_empty() {
                continue;
            }
            let req = self.create_request(batch.clone());
            set.spawn(async move { Self::send(req).await });
        }

        for result in set.join_all().await {
            if let Err(e) = result {
                debug!("Failed to send logs: {}", e);
            }
        }
    }

    fn create_request(&self, data: Vec<u8>) -> ResponseFuture {
        let url = format!("{}/api/v2/logs", self.fqdn_site);
        let body = self.compress(data);

        let mut req = Request::builder()
            .method(Method::POST)
            .uri(url)
            .body(Body::from(body))
            .expect("failed to build request");
        req.headers_mut().extend(self.headers.clone());

        self.client.request(req)
    }

    async fn send(req: ResponseFuture) -> Result<(), Box<dyn Error + Send>> {
        let time = Instant::now();
        let resp = req.await;
        let elapsed = time.elapsed();
        println!("Log Flush time: {elapsed:?}");

        match resp {
            Ok(resp) => {
                let status = resp.status();
                if status != 202 {
                    debug!(
                        "Failed to send logs to datadog after {}ms: {}",
                        elapsed.as_millis(),
                        status
                    );
                }
            }
            Err(e) => {
                error!(
                    "Failed to send logs to datadog after {}ms: {}",
                    elapsed.as_millis(),
                    e
                );

                return Err(Box::new(e));
            }
        }

        Ok(())
    }

    fn compress(&self, data: Vec<u8>) -> Vec<u8> {
        if !self.config.logs_config_use_compression {
            return data;
        }

        match self.encode(&data) {
            Ok(compressed_data) => compressed_data,
            Err(e) => {
                debug!("Failed to compress data: {}", e);
                data
            }
        }
    }

    fn encode(&self, data: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut encoder = Encoder::new(Vec::new(), self.config.logs_config_compression_level)?;
        encoder.write_all(data)?;
        encoder.finish().map_err(|e| Box::new(e) as Box<dyn Error>)
    }
}
