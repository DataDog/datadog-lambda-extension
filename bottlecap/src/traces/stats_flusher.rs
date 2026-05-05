// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;

use crate::FLUSH_RETRY_COUNT;
use crate::config;
use crate::lifecycle::invocation::processor::S_TO_MS;
use crate::traces::http_client::HttpClient;
use crate::traces::stats_aggregator::StatsAggregator;
use bytes::Bytes;
use dogstatsd::api_key::ApiKeyFactory;
use libdd_capabilities::http::HttpClientTrait;
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

        for attempt in 1..=FLUSH_RETRY_COUNT {
            let start = std::time::Instant::now();
            let resp = send_stats_payload(
                &self.http_client,
                endpoint,
                api_key.as_str(),
                serialized_stats_payload.clone(),
            )
            .await;
            let elapsed = start.elapsed();

            match resp {
                Ok(()) => {
                    debug!(
                        "STATS | Successfully flushed stats to {} in {} ms (attempt {attempt}/{FLUSH_RETRY_COUNT})",
                        endpoint.url,
                        elapsed.as_millis()
                    );
                    return None;
                }
                Err(e) => {
                    debug!(
                        "STATS | Failed to send stats to {} in {} ms (attempt {attempt}/{FLUSH_RETRY_COUNT}): {e:?}",
                        endpoint.url,
                        elapsed.as_millis()
                    );
                }
            }
        }

        error!("STATS | Exhausted all {FLUSH_RETRY_COUNT} attempts, returning stats for redrive");
        Some(stats)
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

/// Posts a serialized stats payload using the supplied client.
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
) -> anyhow::Result<()> {
    let req = http::Request::builder()
        .method(http::Method::POST)
        .uri(target.url.clone())
        .header("Content-Type", "application/msgpack")
        .header("Content-Encoding", "gzip")
        .header("DD-API-KEY", api_key)
        .body(Bytes::from(data))?;

    let response = tokio::time::timeout(
        std::time::Duration::from_millis(target.timeout_ms),
        client.request(req),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Stats request timed out after {} ms", target.timeout_ms))?
    .map_err(|e| anyhow::anyhow!("Failed to send trace stats: {e}"))?;

    let status = response.status();
    if status != http::StatusCode::ACCEPTED {
        let body = response.into_body();
        let preview_len = body.len().min(ERROR_BODY_PREVIEW_BYTES);
        let preview = String::from_utf8_lossy(&body[..preview_len]);
        let truncated = if body.len() > preview_len {
            " (truncated)"
        } else {
            ""
        };
        anyhow::bail!("Server did not accept trace stats (status {status}): {preview}{truncated}");
    }
    Ok(())
}
