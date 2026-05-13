use dogstatsd::api_key::ApiKeyFactory;
use reqwest::header::HeaderMap;
use std::{error::Error, sync::Arc};
use thiserror::Error as ThisError;
use tokio::sync::OnceCell;
use tokio::{sync::Mutex, task::JoinSet};

use tracing::{debug, error, info};

use crate::{
    config,
    flushing::{InvocationDeadline, compute_flush_cap_from},
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
    /// Shared current Lambda invocation deadline (epoch ms). Read at send
    /// time to derive an adaptive per-request timeout via `compute_flush_cap`.
    invocation_deadline: InvocationDeadline,
}

impl Flusher {
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator: Arc<Mutex<Aggregator>>,
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
        client: reqwest::Client,
        invocation_deadline: InvocationDeadline,
    ) -> Self {
        Flusher {
            client,
            aggregator,
            config,
            tags_provider,
            api_key_factory,
            headers: OnceCell::new(),
            invocation_deadline,
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

        let max_attempts = self.config.flush_retry_attempts;
        for request in requests {
            let deadline = self.invocation_deadline.clone();
            let config = self.config.clone();
            join_set.spawn(
                async move { Self::send(request, max_attempts, deadline, config).await },
            );
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

    async fn send(
        request: reqwest::RequestBuilder,
        max_attempts: u32,
        invocation_deadline: InvocationDeadline,
        config: Arc<config::Config>,
    ) -> Result<(), Box<dyn Error + Send>> {
        debug!("PROXY_FLUSHER | Attempting to send request");
        let mut attempts: u32 = 0;

        loop {
            attempts += 1;

            // Compute the per-attempt cap *fresh* each iteration so retries
            // tighten as the Lambda deadline approaches. When there's no budget
            // left we bail immediately so the payload can be queued for retry
            // on the next invocation instead of risking a Lambda timeout.
            let cap = compute_flush_cap_from(
                &invocation_deadline,
                config.flush_timeout,
                config.flush_deadline_margin_ms,
            );
            if cap.is_zero() {
                debug!(
                    "PROXY_FLUSHER | Insufficient remaining invocation budget for attempt {attempts}; deferring for retry"
                );
                return Err(Box::new(FailedProxyRequestError {
                    request,
                    message: format!(
                        "Deferred after {attempts} attempts: no remaining invocation budget"
                    ),
                }));
            }

            let Some(cloned_request) = request.try_clone() else {
                return Err(Box::new(std::io::Error::other("can't clone proxy request")));
            };

            let time = std::time::Instant::now();
            let response = tokio::time::timeout(cap, cloned_request.send()).await;
            let elapsed = time.elapsed();

            match response {
                Ok(Ok(r)) => {
                    let url = r.url().to_string();
                    let status = r.status();
                    let body = r.text().await;
                    if status == 202 || status == 200 {
                        debug!(
                            "PROXY_FLUSHER | Successfully sent request in {} ms to {url}",
                            elapsed.as_millis()
                        );
                        return Ok(());
                    } else if attempts >= max_attempts {
                        // Final attempt. Log with error level and return error.
                        let body_string = body.unwrap_or_default();
                        error!(
                            "PROXY_FLUSHER | Request failed with status {status} to {url}: {body_string} after {attempts} attempts"
                        );
                        return Err(Box::new(FailedProxyRequestError {
                            request,
                            message: format!("Request failed with status {status}: {body_string}"),
                        }));
                    }
                    // Not the final attempt. Log with info level and retry.
                    info!(
                        "PROXY_FLUSHER | Request failed with status {status} to {url}: {body:?} (attempt {attempts}/{max_attempts})"
                    );
                }
                Ok(Err(e)) => {
                    if attempts >= max_attempts {
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
                Err(_elapsed) => {
                    // Per-request deadline elapsed (tokio::time::timeout fired).
                    if attempts >= max_attempts {
                        error!(
                            "PROXY_FLUSHER | Request timed out after {} ms (cap {} ms) on attempt {} of {}",
                            elapsed.as_millis(),
                            cap.as_millis(),
                            attempts,
                            max_attempts
                        );
                        return Err(Box::new(FailedProxyRequestError {
                            request,
                            message: format!(
                                "Timed out after {attempts} attempts (cap {} ms)",
                                cap.as_millis()
                            ),
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
