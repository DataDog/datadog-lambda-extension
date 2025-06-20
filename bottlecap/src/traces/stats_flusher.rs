// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config;
use crate::traces::stats_aggregator::StatsAggregator;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::{config_utils::trace_stats_url, stats_utils};
use ddcommon::Endpoint;
use tracing::{debug, error};

#[async_trait]
pub trait StatsFlusher {
    fn new(
        api_key: String,
        aggregator: Arc<Mutex<StatsAggregator>>,
        config: Arc<config::Config>,
    ) -> Self
    where
        Self: Sized;
    /// Flushes stats to the Datadog trace stats intake.
    async fn send(&self, traces: Vec<pb::ClientStatsPayload>);

    async fn flush(&self);
}

#[allow(clippy::module_name_repetitions)]
#[derive(Clone)]
pub struct ServerlessStatsFlusher {
    // pub buffer: Arc<Mutex<Vec<pb::ClientStatsPayload>>>,
    aggregator: Arc<Mutex<StatsAggregator>>,
    config: Arc<config::Config>,
    endpoint: Endpoint,
}

#[async_trait]
impl StatsFlusher for ServerlessStatsFlusher {
    fn new(
        api_key: String,
        aggregator: Arc<Mutex<StatsAggregator>>,
        config: Arc<config::Config>,
    ) -> Self {
        let stats_url = trace_stats_url(&config.site);

        let endpoint = Endpoint {
            url: hyper::Uri::from_str(&stats_url).expect("can't make URI from stats url, exiting"),
            api_key: Some(api_key.clone().into()),
            timeout_ms: config.flush_timeout * 1_000,
            test_token: None,
        };

        ServerlessStatsFlusher {
            aggregator,
            config,
            endpoint,
        }
    }

    async fn send(&self, stats: Vec<pb::ClientStatsPayload>) {
        if stats.is_empty() {
            return;
        }
        debug!("Flushing {} stats", stats.len());

        let stats_payload = stats_utils::construct_stats_payload(stats);

        debug!("Stats payload to be sent: {stats_payload:?}");

        let serialized_stats_payload = match tokio::task::spawn_blocking(move || {
            stats_utils::serialize_stats_payload(stats_payload)
        })
        .await
        {
            Ok(Ok(res)) => res,
            Ok(Err(err)) => {
                error!("Failed to serialize stats payload, dropping stats: {err}");
                return;
            }
            Err(err) => {
                error!("Failed to spawn serialization task: {err}");
                return;
            }
        };

        let stats_url = trace_stats_url(&self.config.site);

        let start = std::time::Instant::now();

        let resp = stats_utils::send_stats_payload(
            serialized_stats_payload,
            &self.endpoint,
            &self.config.api_key,
        )
        .await;
        let elapsed = start.elapsed();
        debug!(
            "Stats request to {} took {}ms",
            stats_url,
            elapsed.as_millis()
        );
        match resp {
            Ok(()) => debug!("Successfully flushed stats"),
            Err(e) => {
                error!("Error sending stats: {e:?}");
            }
        };
    }
    async fn flush(&self) {
        let mut guard = self.aggregator.lock().await;

        let mut stats = guard.get_batch();
        while !stats.is_empty() {
            self.send(stats).await;

            stats = guard.get_batch();
        }
    }
}
