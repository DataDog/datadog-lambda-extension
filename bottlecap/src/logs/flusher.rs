use crate::FLUSH_RETRY_COUNT;
use crate::config;
use crate::http::get_client;
use crate::logs::aggregator::Aggregator;
use dogstatsd::api_key::ApiKeyFactory;
use futures::future::join_all;
use reqwest::header::HeaderMap;
use std::error::Error;
use std::time::Instant;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};
use thiserror::Error as ThisError;
use tokio::sync::OnceCell;
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
    endpoint: String,
    aggregator: Arc<Mutex<Aggregator>>,
    config: Arc<config::Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    headers: OnceCell<HeaderMap>,
}

impl Flusher {
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        endpoint: String,
        aggregator: Arc<Mutex<Aggregator>>,
        config: Arc<config::Config>,
    ) -> Self {
        let client = get_client(&config);
        Flusher {
            client,
            endpoint,
            aggregator,
            config,
            api_key_factory,
            headers: OnceCell::new(),
        }
    }

    pub async fn flush(&self, batches: Option<Arc<Vec<Vec<u8>>>>) -> Vec<reqwest::RequestBuilder> {
        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!("Skipping flushing logs: Failed to resolve API key");
            return vec![];
        };

        let mut set = JoinSet::new();

        if let Some(logs_batches) = batches {
            for batch in logs_batches.iter() {
                if batch.is_empty() {
                    continue;
                }
                let req = self.create_request(batch.clone(), api_key).await;
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

    async fn create_request(&self, data: Vec<u8>, api_key: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/api/v2/logs", self.endpoint);
        let headers = self.get_headers(api_key).await;
        self.client
            .post(&url)
            .timeout(std::time::Duration::from_secs(self.config.flush_timeout))
            .headers(headers.clone())
            .body(data)
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

    async fn get_headers(&self, api_key: &str) -> &HeaderMap {
        self.headers
            .get_or_init(move || async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    "DD-API-KEY",
                    api_key.parse().expect("failed to parse header"),
                );
                headers.insert(
                    "DD-PROTOCOL",
                    "agent-json".parse().expect("failed to parse header"),
                );
                headers.insert(
                    "Content-Type",
                    "application/json".parse().expect("failed to parse header"),
                );

                if self.config.logs_config_use_compression {
                    headers.insert(
                        "Content-Encoding",
                        "zstd".parse().expect("failed to parse header"),
                    );
                }
                headers
            })
            .await
    }
}

#[allow(clippy::module_name_repetitions)]
#[derive(Clone)]
pub struct LogsFlusher {
    config: Arc<config::Config>,
    pub flushers: Vec<Flusher>,
}

impl LogsFlusher {
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator: Arc<Mutex<Aggregator>>,
        config: Arc<config::Config>,
    ) -> Self {
        let mut flushers = Vec::new();

        // Create primary flusher
        flushers.push(Flusher::new(
            Arc::clone(&api_key_factory),
            config.logs_config_logs_dd_url.clone(),
            aggregator.clone(),
            config.clone(),
        ));

        // Create flushers for additional endpoints
        for endpoint in &config.logs_config_additional_endpoints {
            let endpoint_url = format!("https://{}:{}", endpoint.host, endpoint.port);
            flushers.push(Flusher::new(
                Arc::clone(&api_key_factory),
                endpoint_url,
                aggregator.clone(),
                config.clone(),
            ));
        }

        LogsFlusher { config, flushers }
    }

    pub async fn flush(
        &self,
        retry_request: Option<reqwest::RequestBuilder>,
    ) -> Vec<reqwest::RequestBuilder> {
        let mut failed_requests = Vec::new();

        // If retry_request is provided, only process that request
        if let Some(req) = retry_request {
            if let Some(req_clone) = req.try_clone() {
                if let Err(e) = Flusher::send(req_clone).await {
                    if let Some(failed_req_err) = e.downcast_ref::<FailedRequestError>() {
                        failed_requests.push(
                            failed_req_err
                                .request
                                .try_clone()
                                .expect("should be able to clone request"),
                        );
                    }
                }
            }
        } else {
            // Get batches from primary flusher's aggregator
            let logs_batches = Arc::new({
                let mut guard = self.flushers[0].aggregator.lock().expect("lock poisoned");
                let mut batches = Vec::new();
                let mut current_batch = guard.get_batch();
                while !current_batch.is_empty() {
                    batches.push(self.compress(current_batch));
                    current_batch = guard.get_batch();
                }
                batches
            });

            // Send batches to each flusher
            let futures = self.flushers.iter().map(|flusher| {
                let batches = Arc::clone(&logs_batches);
                let flusher = flusher.clone();
                async move { flusher.flush(Some(batches)).await }
            });

            let results = join_all(futures).await;
            for failed in results {
                failed_requests.extend(failed);
            }
        }
        failed_requests
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
