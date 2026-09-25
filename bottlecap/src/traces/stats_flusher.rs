// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;

use rand::Rng;

use crate::FLUSH_RETRY_COUNT;
use crate::config;
use crate::lifecycle::invocation::processor::S_TO_MS;
use crate::traces::http_client::HttpClient;
use crate::traces::stats_aggregator::StatsAggregator;
use bytes::Bytes;
use dogstatsd::api_key::ApiKeyFactory;
use libdd_capabilities::http::HttpClientCapability;
use libdd_common::Endpoint;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::stats_utils;
use tracing::{debug, error};

pub struct StatsFlusher {
    aggregator: Arc<Mutex<StatsAggregator>>,
    config: Arc<config::Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    stats_url: String,
    endpoint: OnceCell<Endpoint>,
    http_client: HttpClient,
}

impl StatsFlusher {
    #[must_use]
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator: Arc<Mutex<StatsAggregator>>,
        config: Arc<config::Config>,
        http_client: HttpClient,
        stats_url: String,
    ) -> Self {
        StatsFlusher {
            aggregator,
            config,
            api_key_factory,
            stats_url,
            endpoint: OnceCell::new(),
            http_client,
        }
    }

    /// Flushes stats to the Datadog trace stats intake.
    ///
    /// Returns `None` on success, or `Some(failed_stats)` if the flush failed and should be retried.
    pub async fn send(
        &self,
        stats: Vec<pb::ClientStatsPayload>,
    ) -> Option<Vec<pb::ClientStatsPayload>> {
        if stats.is_empty() {
            return None;
        }

        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!("STATS | Skipping flushing stats: Failed to resolve API key");
            // No API key means we can't send - don't retry as it won't help
            return None;
        };

        let api_key_clone = api_key.clone();
        let endpoint = self
            .endpoint
            .get_or_init({
                move || async move {
                    Endpoint {
                        url: hyper::Uri::from_str(&self.stats_url)
                            .expect("can't make URI from stats url, exiting"),
                        api_key: Some(api_key_clone.into()),
                        timeout_ms: self.config.flush_timeout * S_TO_MS,
                        test_token: None,
                        use_system_resolver: false,
                    }
                }
            })
            .await;

        debug!("STATS | Flushing {} stats", stats.len());

        let stats_payload = stats_utils::construct_stats_payload(stats.clone());

        debug!("STATS | Stats payload to be sent: {stats_payload:?}");

        let serialized_stats_payload = match stats_utils::serialize_stats_payload(stats_payload) {
            Ok(res) => res,
            Err(err) => {
                // Serialization errors are permanent - data is malformed, don't retry
                error!("STATS | Failed to serialize stats payload, dropping stats: {err}");
                return None;
            }
        };

        // Backoff budget: at most 50 + 100 = 150 ms per send round (full
        // jitter over an exponential base), small next to the per-attempt
        // timeout (flush_timeout seconds), which dominates failure latency
        // on Lambda.
        // Permanent failures are dropped: retrying cannot fix them, neither
        // locally nor via redrive. Retriable failures that exhausted their
        // attempts are returned for one more flush round.
        match send_with_retry(
            &self.http_client,
            endpoint,
            api_key.as_str(),
            serialized_stats_payload,
            Backoff {
                base: Duration::from_millis(BACKOFF_BASE_MS),
            },
        )
        .await
        {
            SendResult::Retriable => Some(stats),
            SendResult::Delivered | SendResult::Permanent => None,
        }
    }

    /// Flushes stats from the aggregator.
    ///
    /// Returns `None` on success, or `Some(failed_stats)` if any flush failed and should be retried.
    /// If `failed_stats` is provided, it will attempt to send those first before fetching new stats.
    pub async fn flush(
        &self,
        force_flush: bool,
        failed_stats: Option<Vec<pb::ClientStatsPayload>>,
    ) -> Option<Vec<pb::ClientStatsPayload>> {
        let mut all_failed: Vec<pb::ClientStatsPayload> = Vec::new();

        // First, retry any previously failed stats
        if let Some(retry_stats) = failed_stats
            && !retry_stats.is_empty()
        {
            debug!(
                "STATS | Retrying {} previously failed stats",
                retry_stats.len()
            );
            if let Some(still_failed) = self.send(retry_stats).await {
                all_failed.extend(still_failed);
            }
        }

        // Then flush new stats from the aggregator
        let mut guard = self.aggregator.lock().await;
        let mut stats = guard.get_batch(force_flush).await;
        while !stats.is_empty() {
            if let Some(failed) = self.send(stats).await {
                all_failed.extend(failed);
            }
            stats = guard.get_batch(force_flush).await;
        }

        if all_failed.is_empty() {
            None
        } else {
            Some(all_failed)
        }
    }
}

/// Maximum number of body bytes to surface in error messages.
const ERROR_BODY_PREVIEW_BYTES: usize = 512;

/// Base for the full-jitter exponential backoff between retries, in ms.
const BACKOFF_BASE_MS: u64 = 50;

/// Whether a status code counts as delivered, retriable, or permanent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Classification {
    Success,
    Retriable,
    Permanent,
}

/// Result of a single send attempt.
enum SendOutcome {
    Success,
    /// Transport error, per-attempt timeout, or retriable status (408, 425,
    /// 429, 5xx), with detail.
    Retriable(String),
    /// Non-retriable status or request-build error, with detail.
    Permanent(String),
}

/// Outcome of a full `send_with_retry` round, after all local attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendResult {
    /// Delivered on some attempt.
    Delivered,
    /// Retriable failure (transport error, timeout, or 408/425/429/5xx) that
    /// exhausted the local attempts; the caller may still redrive the payload.
    Retriable,
    /// Permanent failure (other 4xx status or request-build error); retrying
    /// cannot succeed, so the payload must not be redriven.
    Permanent,
}

/// Full-jitter exponential backoff: the delay before retry `retry`
/// (1-based) is drawn uniformly from `0..=base * 2^(retry-1)`.
struct Backoff {
    base: Duration,
}

impl Backoff {
    /// Inclusive upper bound for the delay before retry `retry`.
    fn upper_bound(&self, retry: u32) -> Duration {
        self.base * 2u32.pow(retry - 1)
    }

    /// Draws the actual delay for retry `retry` from `rng`.
    fn delay(&self, rng: &mut impl Rng, retry: u32) -> Duration {
        let upper_ms = u64::try_from(self.upper_bound(retry).as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(rng.gen_range(0..=upper_ms))
    }
}

/// Mirrors the Go agent's `isRetriableStatus`: any 2xx is success, 408, 425,
/// 429 and 500-599 are retriable, everything else is permanent.
fn classify_status(status: http::StatusCode) -> Classification {
    if status.is_success() {
        Classification::Success
    } else if status.as_u16() == 408
        || status.as_u16() == 425
        || status.as_u16() == 429
        || status.is_server_error()
    {
        Classification::Retriable
    } else {
        Classification::Permanent
    }
}

/// Renders at most `ERROR_BODY_PREVIEW_BYTES` of a response body, noting
/// truncation.
fn body_preview(body: &[u8]) -> String {
    let preview_len = body.len().min(ERROR_BODY_PREVIEW_BYTES);
    let preview = String::from_utf8_lossy(&body[..preview_len]);
    let truncated = if body.len() > preview_len {
        " (truncated)"
    } else {
        ""
    };
    format!("{preview}{truncated}")
}

/// Sends the payload with up to `FLUSH_RETRY_COUNT` attempts, retrying only
/// retriable failures and backing off with full jitter between attempts.
async fn send_with_retry(
    client: &HttpClient,
    target: &Endpoint,
    api_key: &str,
    data: Vec<u8>,
    backoff: Backoff,
) -> SendResult {
    for attempt in 1..=FLUSH_RETRY_COUNT {
        let start = std::time::Instant::now();
        let outcome = send_stats_payload(client, target, api_key, data.clone()).await;
        let elapsed = start.elapsed();

        match outcome {
            SendOutcome::Success => {
                debug!(
                    "STATS | Successfully flushed stats to {} in {} ms (attempt {attempt}/{FLUSH_RETRY_COUNT})",
                    target.url,
                    elapsed.as_millis()
                );
                return SendResult::Delivered;
            }
            SendOutcome::Permanent(detail) => {
                error!(
                    "STATS | Permanent failure sending stats to {} (attempt {attempt}/{FLUSH_RETRY_COUNT}): {detail}; not retrying",
                    target.url
                );
                return SendResult::Permanent;
            }
            SendOutcome::Retriable(detail) => {
                debug!(
                    "STATS | Failed to send stats to {} in {} ms (attempt {attempt}/{FLUSH_RETRY_COUNT}): {detail}",
                    target.url,
                    elapsed.as_millis()
                );
                if attempt < FLUSH_RETRY_COUNT {
                    let retry = u32::try_from(attempt).unwrap_or(u32::MAX);
                    let delay = backoff.delay(&mut rand::thread_rng(), retry);
                    debug!("STATS | Retrying stats flush in {} ms", delay.as_millis());
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    error!("STATS | Exhausted all {FLUSH_RETRY_COUNT} attempts, returning stats for redrive");
    SendResult::Retriable
}

/// Posts a serialized stats payload once using the supplied client.
///
/// Equivalent to libdatadog's `stats_utils::send_stats_payload`, but uses the
/// caller-provided client so bottlecap's proxy/TLS configuration is preserved,
/// and enforces `target.timeout_ms` on each attempt so the surrounding retry
/// loop stays bounded by configuration.
async fn send_stats_payload(
    client: &HttpClient,
    target: &Endpoint,
    api_key: &str,
    data: Vec<u8>,
) -> SendOutcome {
    let req = match http::Request::builder()
        .method(http::Method::POST)
        .uri(target.url.clone())
        .header("Content-Type", "application/msgpack")
        .header("Content-Encoding", "gzip")
        .header("DD-API-KEY", api_key)
        .body(Bytes::from(data))
    {
        Ok(req) => req,
        Err(e) => return SendOutcome::Permanent(format!("Failed to build stats request: {e}")),
    };

    let response = match tokio::time::timeout(
        std::time::Duration::from_millis(target.timeout_ms),
        client.request(req),
    )
    .await
    {
        Err(_) => {
            return SendOutcome::Retriable(format!(
                "Stats request timed out after {} ms",
                target.timeout_ms
            ));
        }
        Ok(Err(e)) => return SendOutcome::Retriable(format!("Failed to send trace stats: {e}")),
        Ok(Ok(response)) => response,
    };

    let status = response.status();
    match classify_status(status) {
        Classification::Success => SendOutcome::Success,
        Classification::Retriable => {
            // Retry-After is ignored for pacing (the backoff budget is far
            // smaller than any realistic value), but surfaced for debugging.
            let retry_after = if status.as_u16() == 429 {
                response
                    .headers()
                    .get("Retry-After")
                    .map(|v| format!(", Retry-After: {v:?}"))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let body = response.into_body();
            SendOutcome::Retriable(format!(
                "Server did not accept trace stats (status {status}){retry_after}: {}",
                body_preview(&body)
            ))
        }
        Classification::Permanent => {
            let body = response.into_body();
            SendOutcome::Permanent(format!(
                "Server did not accept trace stats (status {status}): {}",
                body_preview(&body)
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::traces::http_client::create_client;
    use crate::traces::stats_concentrator_service::StatsConcentratorService;
    use httpmock::prelude::*;

    fn test_endpoint(url: hyper::Uri) -> Endpoint {
        Endpoint {
            url,
            api_key: Some("test-api-key".into()),
            timeout_ms: 2_000,
            test_token: None,
            use_system_resolver: false,
        }
    }

    fn mock_endpoint(server: &MockServer) -> Endpoint {
        test_endpoint(hyper::Uri::from_str(&server.url("/api/v0.2/stats")).expect("valid URI"))
    }

    fn zero_backoff() -> Backoff {
        Backoff {
            base: Duration::ZERO,
        }
    }

    #[test]
    fn classify_status_treats_any_2xx_as_success() {
        for code in [200u16, 201, 202, 204, 299] {
            let status = http::StatusCode::from_u16(code).expect("valid code");
            assert_eq!(
                classify_status(status),
                Classification::Success,
                "{status} should be success"
            );
        }
    }

    #[test]
    fn classify_status_marks_retriable_statuses() {
        for code in [408u16, 425, 429, 500, 502, 503, 599] {
            let status = http::StatusCode::from_u16(code).expect("valid code");
            assert_eq!(
                classify_status(status),
                Classification::Retriable,
                "{status} should be retriable"
            );
        }
    }

    #[test]
    fn classify_status_marks_other_4xx_permanent() {
        for code in [400u16, 401, 403, 404, 413] {
            let status = http::StatusCode::from_u16(code).expect("valid code");
            assert_eq!(
                classify_status(status),
                Classification::Permanent,
                "{status} should be permanent"
            );
        }
    }

    #[test]
    fn backoff_upper_bound_doubles_per_retry() {
        let backoff = Backoff {
            base: Duration::from_millis(50),
        };
        assert_eq!(backoff.upper_bound(1), Duration::from_millis(50));
        assert_eq!(backoff.upper_bound(2), Duration::from_millis(100));
        assert_eq!(backoff.upper_bound(3), Duration::from_millis(200));
    }

    #[test]
    fn backoff_delay_stays_within_upper_bound() {
        let backoff = Backoff {
            base: Duration::from_millis(50),
        };
        let mut rng = rand::thread_rng();
        for retry in 1..=3u32 {
            let upper = backoff.upper_bound(retry);
            for _ in 0..1_000 {
                let delay = backoff.delay(&mut rng, retry);
                assert!(
                    delay <= upper,
                    "sampled delay {delay:?} exceeds upper bound {upper:?}"
                );
            }
        }
    }

    async fn assert_flush(status: u16, expected_result: SendResult, expected_hits: usize) {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/api/v0.2/stats");
            then.status(status);
        });
        let client = create_client(None, None, false).expect("client should build");
        let result = send_with_retry(
            &client,
            &mock_endpoint(&server),
            "test-api-key",
            b"stats".to_vec(),
            zero_backoff(),
        )
        .await;

        assert_eq!(result, expected_result);
        assert_eq!(mock.hits(), expected_hits);
    }

    #[tokio::test]
    async fn send_with_retry_accepts_202_immediately() {
        assert_flush(202, SendResult::Delivered, 1).await;
    }

    /// Regression: a 200 from a proxy must count as delivered. Previously
    /// only 202 was accepted, so the payload was resent and double-counted
    /// by the intake.
    #[tokio::test]
    async fn send_with_retry_accepts_any_2xx_immediately() {
        assert_flush(200, SendResult::Delivered, 1).await;
    }

    #[tokio::test]
    async fn send_with_retry_does_not_retry_permanent_400() {
        assert_flush(400, SendResult::Permanent, 1).await;
    }

    #[tokio::test]
    async fn send_with_retry_exhausts_retries_on_503() {
        assert_flush(503, SendResult::Retriable, FLUSH_RETRY_COUNT).await;
    }

    #[tokio::test]
    async fn send_with_retry_exhausts_retries_on_429() {
        assert_flush(429, SendResult::Retriable, FLUSH_RETRY_COUNT).await;
    }

    #[tokio::test]
    async fn send_with_retry_marks_transport_error_retriable() {
        // Bind a listener, read its port, and drop it, so connections to the
        // port are refused.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        drop(listener);

        let url = hyper::Uri::from_str(&format!("http://127.0.0.1:{port}/api/v0.2/stats"))
            .expect("valid URI");
        let client = create_client(None, None, false).expect("client should build");

        let delivered = tokio::time::timeout(
            Duration::from_secs(5),
            send_with_retry(
                &client,
                &test_endpoint(url),
                "test-api-key",
                b"stats".to_vec(),
                zero_backoff(),
            ),
        )
        .await
        .expect("send_with_retry must terminate on connection refusal");

        assert_eq!(
            delivered,
            SendResult::Retriable,
            "transport failure should stay eligible for redrive"
        );
    }

    /// Runs one `send` round against a mock returning `status` and returns
    /// whether the stats were kept for redrive.
    async fn send_once(status: u16) -> Option<Vec<pb::ClientStatsPayload>> {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/api/v0.2/stats");
            then.status(status);
        });
        let client = create_client(None, None, false).expect("client should build");
        let config = Arc::new(Config::default());
        let aggregator = Arc::new(Mutex::new(StatsAggregator::new_with_concentrator(
            StatsConcentratorService::new(Arc::clone(&config)).1,
        )));
        let flusher = StatsFlusher::new(
            Arc::new(ApiKeyFactory::new("test-api-key")),
            aggregator,
            config,
            client,
            format!("{}/api/v0.2/stats", server.url("")),
        );
        flusher.send(vec![pb::ClientStatsPayload::default()]).await
    }

    /// Regression: a permanent status must not be returned for redrive, even
    /// though the payload failed to send.
    #[tokio::test]
    async fn send_drops_stats_on_permanent_failure() {
        let result = tokio::time::timeout(Duration::from_secs(5), send_once(400))
            .await
            .expect("send must terminate");
        assert!(result.is_none(), "permanent failure must be dropped");
    }

    #[tokio::test]
    async fn send_keeps_stats_on_exhausted_retriable_failure() {
        let result = tokio::time::timeout(Duration::from_secs(5), send_once(503))
            .await
            .expect("send must terminate");
        assert!(result.is_some(), "retriable failure must be kept for redrive");
    }
}
