use crate::FLUSH_RETRY_COUNT;
use crate::config;
use crate::http::get_client;
use crate::logs::aggregator_service::AggregatorHandle;
use dogstatsd::api_key::ApiKeyFactory;
use futures::future::join_all;
use hyper::StatusCode;
use reqwest::header::HeaderMap;
use std::error::Error;
use std::time::Instant;
use std::{io::Write, sync::Arc};
use thiserror::Error as ThisError;
use tokio::{sync::OnceCell, task::JoinSet};
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
    config: Arc<config::Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    headers: OnceCell<HeaderMap>,
}

impl Flusher {
    #[must_use]
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        endpoint: String,
        config: Arc<config::Config>,
    ) -> Self {
        let client = get_client(&config);
        Flusher {
            client,
            endpoint,
            config,
            api_key_factory,
            headers: OnceCell::new(),
        }
    }

    pub async fn flush(&self, batches: Option<Arc<Vec<Vec<u8>>>>) -> Vec<reqwest::RequestBuilder> {
        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!("LOGS | Skipping flushing: Failed to resolve API key");
            return vec![];
        };

        let mut set = JoinSet::new();

        if let Some(logs_batches) = batches {
            for batch in logs_batches.iter() {
                if batch.is_empty() {
                    continue;
                }
                let req = self.create_request(batch.clone(), api_key.as_str()).await;
                set.spawn(async move { Self::send(req).await });
            }
        }

        let mut failed_requests = Vec::new();
        for result in set.join_all().await {
            if let Err(e) = result {
                debug!("LOGS | Failed to join task: {}", e);
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
                    debug!("LOGS | Failed to send request after retries, will retry later");
                } else {
                    debug!("LOGS | Failed to send request: {}", e);
                }
            }
        }
        failed_requests
    }

    async fn create_request(&self, data: Vec<u8>, api_key: &str) -> reqwest::RequestBuilder {
        let url = if self.config.observability_pipelines_worker_logs_enabled {
            self.endpoint.clone()
        } else {
            format!("{}/api/v2/logs", self.endpoint)
        };
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
                return Err(Box::new(std::io::Error::other("can't clone")));
            };
            let resp = cloned_req.send().await;
            let elapsed = time.elapsed();

            match resp {
                Ok(resp) => {
                    let status = resp.status();
                    _ = resp.text().await;
                    if status == StatusCode::FORBIDDEN {
                        // Access denied. Stop retrying.
                        error!(
                            "LOGS | Request was denied by Datadog: Access denied. Please verify that your API key is valid."
                        );
                        return Ok(());
                    }
                    if status.is_success() {
                        return Ok(());
                    }
                }
                Err(e) => {
                    if attempts >= FLUSH_RETRY_COUNT {
                        // After 3 failed attempts, return the original request for later retry
                        // Create a custom error that can be downcast to get the RequestBuilder
                        error!(
                            "LOGS | Failed to send request after {} ms and {} attempts: {:?}",
                            elapsed.as_millis(),
                            attempts,
                            e
                        );
                        return Err(Box::new(FailedRequestError {
                            request: req,
                            message: format!("LOGS | Failed after {attempts} attempts: {e}"),
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
                if !self.config.observability_pipelines_worker_logs_enabled {
                    headers.insert(
                        "DD-PROTOCOL",
                        "agent-json".parse().expect("failed to parse header"),
                    );
                }
                headers.insert(
                    "Content-Type",
                    "application/json".parse().expect("failed to parse header"),
                );

                if self.config.logs_config_use_compression
                    && !self.config.observability_pipelines_worker_logs_enabled
                {
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
    aggregator_handle: AggregatorHandle,
}

impl LogsFlusher {
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator_handle: AggregatorHandle,
        config: Arc<config::Config>,
    ) -> Self {
        let mut flushers = Vec::new();

        let endpoint = if config.observability_pipelines_worker_logs_enabled {
            if config.observability_pipelines_worker_logs_url.is_empty() {
                error!("LOGS | Observability Pipelines Worker URL is empty");
            }
            config.observability_pipelines_worker_logs_url.clone()
        } else {
            config.logs_config_logs_dd_url.clone()
        };

        // Create primary flusher
        flushers.push(Flusher::new(
            Arc::clone(&api_key_factory),
            endpoint,
            config.clone(),
        ));

        // Create flushers for additional endpoints
        for endpoint in &config.logs_config_additional_endpoints {
            let endpoint_url = format!("https://{}:{}", endpoint.host, endpoint.port);
            let additional_api_key_factory =
                Arc::new(ApiKeyFactory::new(endpoint.api_key.clone().as_str()));
            flushers.push(Flusher::new(
                additional_api_key_factory,
                endpoint_url,
                config.clone(),
            ));
        }

        LogsFlusher {
            config,
            flushers,
            aggregator_handle,
        }
    }

    pub async fn flush(
        &self,
        retry_request: Option<reqwest::RequestBuilder>,
    ) -> Vec<reqwest::RequestBuilder> {
        let mut failed_requests = Vec::new();

        // If retry_request is provided, only process that request
        if let Some(req) = retry_request {
            if let Some(req_clone) = req.try_clone()
                && let Err(e) = Flusher::send(req_clone).await
                && let Some(failed_req_err) = e.downcast_ref::<FailedRequestError>()
            {
                failed_requests.push(
                    failed_req_err
                        .request
                        .try_clone()
                        .expect("should be able to clone request"),
                );
            }
        } else {
            let logs_batches = Arc::new({
                match self.aggregator_handle.get_batches().await {
                    Ok(batches) => batches
                        .into_iter()
                        .map(|batch| self.compress(batch))
                        .collect(),
                    Err(e) => {
                        debug!("Failed to flush from aggregator: {}", e);
                        Vec::new()
                    }
                }
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
        if !self.config.logs_config_use_compression
            || self.config.observability_pipelines_worker_logs_enabled
        {
            return data;
        }

        match self.encode(&data) {
            Ok(compressed_data) => compressed_data,
            Err(e) => {
                debug!("LOGS | Failed to compress data: {}", e);
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
