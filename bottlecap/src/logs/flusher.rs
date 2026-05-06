use crate::FLUSH_RETRY_COUNT;
use crate::config;
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
        client: reqwest::Client,
    ) -> Self {
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
                    let body = resp.text().await.unwrap_or_default();
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
                    if attempts >= FLUSH_RETRY_COUNT {
                        // Non-success HTTP response (e.g. 4xx/5xx from an OPW or
                        // intake under load) — surface the request for later retry
                        // instead of looping unbounded against a degraded endpoint.
                        error!(
                            "LOGS | Failed to send request after {} ms and {} attempts: status {status}, body: {body}",
                            elapsed.as_millis(),
                            attempts,
                        );
                        return Err(Box::new(FailedRequestError {
                            request: req,
                            message: format!(
                                "LOGS | Failed after {attempts} attempts: status {status}"
                            ),
                        }));
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
        client: reqwest::Client,
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
            client.clone(),
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
                client.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn build_request(server: &MockServer, timeout: Duration) -> reqwest::RequestBuilder {
        reqwest::Client::new()
            .post(server.url("/api/v2/logs"))
            .timeout(timeout)
            .body("test")
    }

    #[tokio::test]
    async fn send_returns_ok_on_success() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v2/logs");
            then.status(200);
        });

        let result = Flusher::send(build_request(&server, Duration::from_secs(2))).await;

        assert!(result.is_ok(), "2xx response should return Ok immediately");
        mock.assert_hits(1);
    }

    #[tokio::test]
    async fn send_returns_ok_on_forbidden_without_retry() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v2/logs");
            then.status(403);
        });

        let result = Flusher::send(build_request(&server, Duration::from_secs(2))).await;

        assert!(
            result.is_ok(),
            "403 is permanent (bad API key) — drop, do not retry"
        );
        mock.assert_hits(1);
    }

    /// Regression test for SLES-2843: a persistent non-success, non-403 status
    /// (e.g. 500 from an OPW under load) must respect `FLUSH_RETRY_COUNT`
    /// instead of looping unbounded and hammering the endpoint.
    #[tokio::test]
    async fn send_bounds_retries_on_non_success_status() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v2/logs");
            then.status(500);
        });

        // Bound the test so the buggy (unbounded) implementation fails fast
        // instead of hanging the suite.
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            Flusher::send(build_request(&server, Duration::from_secs(2))),
        )
        .await
        .expect("send must respect FLUSH_RETRY_COUNT and not loop forever on non-success");

        assert!(
            result.is_err(),
            "send should return Err after exhausting retries on persistent 5xx"
        );
        mock.assert_hits(FLUSH_RETRY_COUNT);
    }

    #[tokio::test]
    async fn send_bounds_retries_on_4xx_status() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v2/logs");
            then.status(404);
        });

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            Flusher::send(build_request(&server, Duration::from_secs(2))),
        )
        .await
        .expect("send must respect FLUSH_RETRY_COUNT on 4xx (e.g. misrouted OPW URL)");

        assert!(
            result.is_err(),
            "persistent 4xx should bound at FLUSH_RETRY_COUNT"
        );
        mock.assert_hits(FLUSH_RETRY_COUNT);
    }

    #[tokio::test]
    async fn send_bounds_retries_on_transport_error() {
        let server = MockServer::start();
        // Server holds the response longer than the client timeout so every
        // attempt resolves to a reqwest timeout error (the `Err` branch).
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v2/logs");
            then.status(200).delay(Duration::from_secs(5));
        });

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            Flusher::send(build_request(&server, Duration::from_millis(50))),
        )
        .await
        .expect("send must respect FLUSH_RETRY_COUNT on transport errors");

        assert!(
            result.is_err(),
            "send should return Err after exhausting retries on transport timeout"
        );
        mock.assert_hits(FLUSH_RETRY_COUNT);
    }

    /// Gap #2: when a transient failure clears, the loop must actually retry
    /// and succeed — not treat the first non-2xx as a permanent failure.
    /// Without this test, a refactor that early-returns on the first failed
    /// status would still pass the bounded-retry tests above.
    ///
    /// Uses an axum-based stateful mock because httpmock 0.7 matchers cannot
    /// capture state (their predicate type is `fn`, not `Fn`).
    #[tokio::test]
    async fn send_succeeds_on_retry_after_transient_failure() {
        use axum::{Router, http::StatusCode, routing::post};
        use std::sync::atomic::AtomicUsize;

        let counter = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/api/v2/logs",
            post({
                let counter = Arc::clone(&counter);
                move || {
                    let counter = Arc::clone(&counter);
                    async move {
                        // First attempt → 500 (transient), all later attempts → 200.
                        let n = counter.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            StatusCode::INTERNAL_SERVER_ERROR
                        } else {
                            StatusCode::OK
                        }
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test should be able to bind a local port");
        let addr = listener
            .local_addr()
            .expect("bound listener should have a local addr");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test server should run cleanly");
        });

        let req = reqwest::Client::new()
            .post(format!("http://{addr}/api/v2/logs"))
            .timeout(Duration::from_secs(2))
            .body("test");

        let result = Flusher::send(req).await;

        assert!(
            result.is_ok(),
            "send must retry past a transient failure and succeed on a later attempt"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "expected exactly one failed attempt + one successful retry"
        );
    }

    /// Gap #1: after retry exhaustion, the returned error must carry a
    /// re-issuable request so `Flusher::flush` can re-queue it for the next
    /// flush cycle. A refactor that loses or corrupts the request would
    /// silently turn a transient failure into permanent data loss.
    #[tokio::test]
    async fn failed_request_error_carries_replayable_request() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v2/logs");
            then.status(500);
        });

        let err = tokio::time::timeout(
            Duration::from_secs(3),
            Flusher::send(build_request(&server, Duration::from_secs(2))),
        )
        .await
        .expect("send must terminate")
        .expect_err("expected Err after retry exhaustion");

        let failed = err
            .downcast_ref::<FailedRequestError>()
            .expect("error must be downcastable to FailedRequestError so flush() can re-queue it");

        let cloned = failed
            .request
            .try_clone()
            .expect("FailedRequestError.request must be cloneable for redrive");

        // Re-issue the stashed request and confirm it actually reaches the
        // server — proves the request is intact, not a corrupted shell.
        let response = cloned
            .send()
            .await
            .expect("re-issued request should be sendable");
        assert_eq!(response.status().as_u16(), 500);

        // FLUSH_RETRY_COUNT attempts during send + 1 from the re-issue above.
        mock.assert_hits(FLUSH_RETRY_COUNT + 1);
    }
}
