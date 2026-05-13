// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;

use crate::config;
use crate::flushing::{InvocationDeadline, compute_flush_cap_from};
use crate::traces::http_client::HttpClient;
use crate::traces::stats_aggregator::StatsAggregator;
use dogstatsd::api_key::ApiKeyFactory;
use libdd_common::Endpoint;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::{config_utils::trace_stats_url, stats_utils};
use tracing::{debug, error};

pub struct StatsFlusher {
    aggregator: Arc<Mutex<StatsAggregator>>,
    config: Arc<config::Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    /// Cached parsed stats-intake URL. Computed once because `Uri::from_str`
    /// allocates and validates; rebuilt into a fresh `Endpoint` per send so
    /// `timeout_ms` can reflect the *current* invocation budget.
    endpoint_url: OnceCell<hyper::Uri>,
    http_client: HttpClient,
    /// Shared current Lambda invocation deadline (epoch ms). Read at send
    /// time to derive an adaptive per-request timeout via `compute_flush_cap`.
    pub invocation_deadline: InvocationDeadline,
}

impl StatsFlusher {
    #[must_use]
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator: Arc<Mutex<StatsAggregator>>,
        config: Arc<config::Config>,
        http_client: HttpClient,
        invocation_deadline: InvocationDeadline,
    ) -> Self {
        StatsFlusher {
            aggregator,
            config,
            api_key_factory,
            endpoint_url: OnceCell::new(),
            http_client,
            invocation_deadline,
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

        let endpoint_url = self
            .endpoint_url
            .get_or_init({
                || async move {
                    let stats_url = trace_stats_url(&self.config.site);
                    hyper::Uri::from_str(&stats_url)
                        .expect("can't make URI from stats url, exiting")
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

        let stats_url = trace_stats_url(&self.config.site);
        let max_attempts = self.config.flush_retry_attempts;

        for attempt in 1..=max_attempts {
            // Compute the per-attempt cap *fresh* so retries tighten as the
            // Lambda deadline approaches. When there's no budget left we bail
            // immediately so the payload can be redriven on the next
            // invocation instead of risking a Lambda timeout.
            let cap = compute_flush_cap_from(
                &self.invocation_deadline,
                self.config.flush_timeout,
                self.config.flush_deadline_margin_ms,
            );
            if cap.is_zero() {
                debug!(
                    "STATS | Insufficient remaining invocation budget on attempt {attempt}; deferring for redrive"
                );
                return Some(stats);
            }

            // Build a fresh Endpoint per attempt so its `timeout_ms` (consumed
            // by `tokio::time::timeout` inside `send_with_retry`) reflects the
            // current remaining budget.
            #[allow(clippy::cast_possible_truncation)]
            let endpoint = Endpoint {
                url: endpoint_url.clone(),
                api_key: Some(api_key.clone().into()),
                timeout_ms: cap.as_millis().min(u64::MAX as u128) as u64,
                test_token: None,
                use_system_resolver: false,
            };

            let start = std::time::Instant::now();
            let resp = stats_utils::send_stats_payload_with_client(
                serialized_stats_payload.clone(),
                &endpoint,
                api_key.as_str(),
                Some(&self.http_client),
            )
            .await;
            let elapsed = start.elapsed();

            match resp {
                Ok(()) => {
                    debug!(
                        "STATS | Successfully flushed stats to {stats_url} in {} ms (attempt {attempt}/{max_attempts}, cap {} ms)",
                        elapsed.as_millis(),
                        cap.as_millis()
                    );
                    return None;
                }
                Err(e) => {
                    debug!(
                        "STATS | Failed to send stats to {stats_url} in {} ms (attempt {attempt}/{max_attempts}, cap {} ms): {e:?}",
                        elapsed.as_millis(),
                        cap.as_millis()
                    );
                }
            }
        }

        error!("STATS | Exhausted all {max_attempts} attempts, returning stats for redrive");
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
