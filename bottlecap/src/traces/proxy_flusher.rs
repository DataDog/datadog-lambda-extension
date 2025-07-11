use std::{error::Error, sync::Arc};
use thiserror::Error as ThisError;
use tokio::{sync::Mutex, task::JoinSet};

use reqwest::header::HeaderMap;
use tracing::{debug, error};

use crate::{
    config,
    http::get_client,
    tags::provider,
    traces::{
        proxy_aggregator::{Aggregator, ProxyRequest},
        DD_ADDITIONAL_TAGS_HEADER,
    },
    FLUSH_RETRY_COUNT,
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
    headers: HeaderMap,
}

impl Flusher {
    pub fn new(
        api_key: String,
        aggregator: Arc<Mutex<Aggregator>>,
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
    ) -> Self {
        let client = get_client(&config);
        let mut headers = HeaderMap::new();
        headers.insert(
            "DD-API-KEY",
            api_key.parse().expect("Failed to parse API key header"),
        );
        let additional_tags = format!(
            "_dd.origin:lambda;functionname:{}",
            tags_provider
                .get_canonical_resource_name()
                .unwrap_or_default()
        );
        headers.insert(
            DD_ADDITIONAL_TAGS_HEADER,
            additional_tags
                .parse()
                .expect("Failed to parse additional tags header"),
        );

        Flusher {
            client,
            aggregator,
            config,
            headers,
        }
    }

    pub async fn flush(
        &self,
        retry_requests: Option<Vec<reqwest::RequestBuilder>>,
    ) -> Option<Vec<reqwest::RequestBuilder>> {
        let mut join_set = JoinSet::new();
        let mut requests = Vec::<reqwest::RequestBuilder>::new();

        // If there are any requests to retry, start with those
        if retry_requests.as_ref().is_some_and(|r| !r.is_empty()) {
            let retries = retry_requests.unwrap_or_default();
            debug!("Proxy Flusher | Retrying {} failed requests", retries.len());
            requests = retries;
        } else {
            let mut aggregator = self.aggregator.lock().await;
            for pr in aggregator.get_batch() {
                requests.push(self.create_request(pr));
            }
        }

        for request in requests {
            join_set.spawn(async move { Self::send(request).await });
        }

        let send_results = join_set.join_all().await;

        Self::get_failed_requests(send_results)
    }

    fn create_request(&self, request: ProxyRequest) -> reqwest::RequestBuilder {
        let mut headers = request.headers.clone();

        // Remove headers that are not needed for the proxy request
        headers.remove("host");
        headers.remove("content-length");

        headers.extend(self.headers.clone());

        self.client
            .post(&request.target_url)
            .headers(headers)
            .timeout(std::time::Duration::from_secs(self.config.flush_timeout))
            .body(request.body)
    }

    async fn send(request: reqwest::RequestBuilder) -> Result<(), Box<dyn Error + Send>> {
        debug!("Proxy Flusher | Attempting to send request");
        let mut attempts = 0;

        loop {
            attempts += 1;

            let Some(cloned_request) = request.try_clone() else {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "can't clone proxy request",
                )));
            };

            let time = std::time::Instant::now();
            let response = cloned_request.send().await;
            let elapsed = time.elapsed();

            match response {
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await;
                    if status == 202 || status == 200 {
                        debug!(
                            "Proxy Flusher | Successfully sent request in {} ms",
                            elapsed.as_millis()
                        );
                    } else {
                        error!("Proxy Flusher | Request failed with status {status}: {body:?}");
                    }

                    return Ok(());
                }
                Err(e) => {
                    if attempts >= FLUSH_RETRY_COUNT {
                        error!(
                            "Proxy Flusher | Failed to send request after {} attempts: {:?}",
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
            if let Err(e) = result {
                if let Some(fpre) = e.downcast_ref::<FailedProxyRequestError>() {
                    if let Some(request) = fpre.request.try_clone() {
                        failed_requests.push(request);
                    }
                }
            }
        }

        if failed_requests.is_empty() {
            return None;
        }

        Some(failed_requests)
    }
}
