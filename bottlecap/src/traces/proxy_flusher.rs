use dogstatsd::api_key::ApiKeyFactory;
use reqwest::header::HeaderMap;
use std::{error::Error, sync::Arc};
use thiserror::Error as ThisError;
use tokio::sync::OnceCell;
use tokio::{sync::Mutex, task::JoinSet};

use tracing::{debug, error};

use crate::{
    FLUSH_RETRY_COUNT, config,
    http::get_client,
    tags::provider,
    traces::{
        DD_ADDITIONAL_TAGS_HEADER,
        proxy_aggregator::{Aggregator, ProxyRequest},
    },
};

#[derive(ThisError, Debug)]
#[error("{message}")]
pub struct FailedProxyRequestError {
    pub request: reqwest::RequestBuilder,
    pub message: String,
}

pub struct Flusher {
    client: reqwest::Client,
    aggregator: Arc<Mutex<Aggregator>>,
    config: Arc<config::Config>,
    tags_provider: Arc<provider::Provider>,
    api_key_factory: Arc<ApiKeyFactory>,
    headers: OnceCell<HeaderMap>,
}

impl Flusher {
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator: Arc<Mutex<Aggregator>>,
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
    ) -> Self {
        let client = get_client(&config);

        Flusher {
            client,
            aggregator,
            config,
            tags_provider,
            api_key_factory,
            headers: OnceCell::new(),
        }
    }

    async fn get_headers(&self, api_key: &str) -> &HeaderMap {
        self.headers
            .get_or_init(move || async move {
                let mut headers = HeaderMap::new();
                headers.insert(
                    "DD-API-KEY",
                    api_key.parse().expect("Failed to parse API key header"),
                );
                let additional_tags = format!(
                    "_dd.origin:lambda;functionname:{}",
                    self.tags_provider
                        .get_canonical_resource_name()
                        .unwrap_or_default()
                );
                headers.insert(
                    DD_ADDITIONAL_TAGS_HEADER,
                    additional_tags
                        .parse()
                        .expect("Failed to parse additional tags header"),
                );
                headers
            })
            .await
    }

    pub async fn flush(
        &self,
        retry_requests: Option<Vec<reqwest::RequestBuilder>>,
    ) -> Option<Vec<reqwest::RequestBuilder>> {
        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!(
                "PROXY_FLUSHER | Failed to resolve API key, dropping aggregated data and skipping flush."
            );
            {
                let mut aggregator = self.aggregator.lock().await;
                aggregator.clear();
            }
            return None;
        };

        let mut join_set = JoinSet::new();
        let mut requests = Vec::<reqwest::RequestBuilder>::new();

        // If there are any requests to retry, start with those
        if retry_requests.as_ref().is_some_and(|r| !r.is_empty()) {
            let retries = retry_requests.unwrap_or_default();
            debug!("PROXY_FLUSHER | Retrying {} failed requests", retries.len());
            requests = retries;
        } else {
            let mut aggregator = self.aggregator.lock().await;
            for pr in aggregator.get_batch() {
                requests.push(self.create_request(pr, api_key.as_str()).await);
            }
        }

        for request in requests {
            join_set.spawn(async move { Self::send(request).await });
        }

        let send_results = join_set.join_all().await;

        Self::get_failed_requests(send_results)
    }

    async fn create_request(
        &self,
        request: ProxyRequest,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        let mut headers = request.headers.clone();

        // Remove headers that are not needed for the proxy request
        headers.remove("host");
        headers.remove("content-length");

        headers.extend(self.get_headers(api_key).await.clone());

        self.client
            .post(&request.target_url)
            .headers(headers)
            .timeout(std::time::Duration::from_secs(self.config.flush_timeout))
            .body(request.body)
    }

    async fn send(request: reqwest::RequestBuilder) -> Result<(), Box<dyn Error + Send>> {
        debug!("PROXY_FLUSHER | Attempting to send request");
        let mut attempts = 0;

        loop {
            attempts += 1;

            let Some(cloned_request) = request.try_clone() else {
                return Err(Box::new(std::io::Error::other("can't clone proxy request")));
            };

            let time = std::time::Instant::now();
            let response = cloned_request.send().await;
            let elapsed = time.elapsed();

            match response {
                Ok(r) => {
                    let url = r.url().to_string();
                    let status = r.status();
                    let body = r.text().await;
                    if status == 202 || status == 200 {
                        debug!(
                            "PROXY_FLUSHER | Successfully sent request in {} ms to {url}",
                            elapsed.as_millis()
                        );
                    } else {
                        error!(
                            "PROXY_FLUSHER | Request failed with status {status} to {url}: {body:?}"
                        );
                    }

                    return Ok(());
                }
                Err(e) => {
                    if attempts >= FLUSH_RETRY_COUNT {
                        error!(
                            "PROXY_FLUSHER | Failed to send request after {} attempts: {:?}",
                            attempts, e
                        );

                        return Err(Box::new(FailedProxyRequestError {
                            request,
                            message: e.to_string(),
                        }));
                    }
                }
            }
        }
    }

    /// Given a vector of results from the `send` method, return a vector of failed requests
    /// if there are any.
    ///
    /// Failed requests should be retried later.
    fn get_failed_requests(
        results: Vec<Result<(), Box<dyn Error + Send>>>,
    ) -> Option<Vec<reqwest::RequestBuilder>> {
        let mut failed_requests: Vec<reqwest::RequestBuilder> = Vec::new();
        for result in results {
            // There is no cleaner way to do this, so it's deeply nested.
            if let Err(e) = result
                && let Some(fpre) = e.downcast_ref::<FailedProxyRequestError>()
                && let Some(request) = fpre.request.try_clone()
            {
                failed_requests.push(request);
            }
        }

        if failed_requests.is_empty() {
            return None;
        }

        Some(failed_requests)
    }
}
