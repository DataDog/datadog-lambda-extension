use crate::config;
use crate::http::get_client;
use crate::logs::aggregator::Aggregator;
use crate::FLUSH_RETRY_COUNT;
use reqwest::header::HeaderMap;
use std::error::Error;
use std::time::Instant;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};
use thiserror::Error as ThisError;
use tokio::task::JoinSet;
use tracing::{debug, error};
use zstd::stream::write::Encoder;

#[derive(ThisError, Debug)]
#[error("{message}")]
pub struct FailedRequestError {
    pub request: reqwest::RequestBuilder,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Flusher {
    client: reqwest::Client,
    aggregator: Arc<Mutex<Aggregator>>,
    config: Arc<config::Config>,
    headers: HeaderMap,
}

impl Flusher {
    pub fn new(
        api_key: String,
        aggregator: Arc<Mutex<Aggregator>>,
        config: Arc<config::Config>,
    ) -> Self {
        let client = get_client(config.clone());
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
            client,
            aggregator,
            config,
            headers,
        }
    }
    pub async fn flush(
        &self,
        retry_request: Option<reqwest::RequestBuilder>,
    ) -> Vec<reqwest::RequestBuilder> {
        let mut set = JoinSet::new();

        // If retry_request is provided, only process that request
        if let Some(req) = retry_request {
            set.spawn(async move { Self::send(req).await });
        } else {
            // Process log batches only if no retry_request is provided
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

            for batch in logs_batches {
                if batch.is_empty() {
                    continue;
                }
                let req = self.create_request(batch);
                set.spawn(async move { Self::send(req).await });
            }
        }

        let mut failed_requests = Vec::new();
        for result in set.join_all().await {
            if let Err(e) = result {
                debug!("Failed to join task: {}", e);
                continue;
            }

            // At this point we know the task completed successfully,
            // but the send operation itself may have failed
            if let Err(e) = result {
                if let Some(failed_req_err) = e.downcast_ref::<FailedRequestError>() {
                    // Clone the request from our custom error
                    failed_requests.push(
                        failed_req_err
                            .request
                            .try_clone()
                            .expect("should be able to clone request"),
                    );
                    debug!("Failed to send logs after retries, will retry later");
                } else {
                    debug!("Failed to send logs: {}", e);
                }
            }
        }
        failed_requests
    }

    fn create_request(&self, data: Vec<u8>) -> reqwest::RequestBuilder {
        let url = format!("{}/api/v2/logs", self.config.logs_config_logs_dd_url);
        let body = self.compress(data);
        self.client
            .post(&url)
            .timeout(std::time::Duration::from_secs(self.config.flush_timeout))
            .headers(self.headers.clone())
            .body(body)
    }

    async fn send(req: reqwest::RequestBuilder) -> Result<(), Box<dyn Error + Send>> {
        let mut attempts = 0;

        loop {
            let time = Instant::now();
            attempts += 1;
            let Some(cloned_req) = req.try_clone() else {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "can't clone",
                )));
            };
            let resp = cloned_req.send().await;
            let elapsed = time.elapsed();

            match resp {
                Ok(resp) => {
                    let status = resp.status();
                    _ = resp.text().await;
                    if status == 202 {
                        return Ok(());
                    }
                }
                Err(e) => {
                    if attempts >= FLUSH_RETRY_COUNT {
                        // After 3 failed attempts, return the original request for later retry
                        // Create a custom error that can be downcast to get the RequestBuilder
                        error!(
                            "Failed to send logs to datadog after {} ms and {} attempts: {:?}",
                            elapsed.as_millis(),
                            attempts,
                            e
                        );
                        return Err(Box::new(FailedRequestError {
                            request: req,
                            message: format!("Failed after {attempts} attempts: {e}"),
                        }));
                    }
                }
            }
        }
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
