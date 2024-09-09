// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::{debug, error};

use crate::config;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::config_utils::trace_stats_url;
use datadog_trace_utils::stats_utils;
use ddcommon::Endpoint;

#[async_trait]
pub trait StatsFlusher {
    /// Starts a stats flusher that listens for stats payloads sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_stats.
    async fn start_stats_flusher(&self, mut rx: Receiver<pb::ClientStatsPayload>);
    /// Flushes stats to the Datadog trace stats intake.
    async fn flush_stats(&self, traces: Vec<pb::ClientStatsPayload>);

    async fn manual_flush(&self);
}

#[allow(clippy::module_name_repetitions)]
#[derive(Clone)]
pub struct ServerlessStatsFlusher {
    pub buffer: Arc<Mutex<Vec<pb::ClientStatsPayload>>>,
    pub config: Arc<config::Config>,
    pub resolved_api_key: String,
}

#[async_trait]
impl StatsFlusher for ServerlessStatsFlusher {
    async fn start_stats_flusher(&self, mut rx: Receiver<pb::ClientStatsPayload>) {
        let buffer_producer = self.buffer.clone();

        tokio::spawn(async move {
            while let Some(stats_payload) = rx.recv().await {
                let mut buffer = buffer_producer.lock().await;
                buffer.push(stats_payload);
            }
        });
    }

    async fn manual_flush(&self) {
        let mut buffer = self.buffer.lock().await;
        if !buffer.is_empty() {
            self.flush_stats(buffer.to_vec()).await;
            buffer.clear();
        }
    }
    async fn flush_stats(&self, stats: Vec<pb::ClientStatsPayload>) {
        if stats.is_empty() {
            return;
        }
        debug!("Flushing {} stats", stats.len());

        let stats_payload = stats_utils::construct_stats_payload(stats);

        debug!("Stats payload to be sent: {stats_payload:?}");

        let serialized_stats_payload = match stats_utils::serialize_stats_payload(stats_payload) {
            Ok(res) => res,
            Err(err) => {
                error!("Failed to serialize stats payload, dropping stats: {err}");
                return;
            }
        };

        let stats_url = trace_stats_url(&self.config.site);

        let endpoint = Endpoint {
            url: hyper::Uri::from_str(&stats_url).expect("can't make URI from stats url, exiting"),
            api_key: Some(self.resolved_api_key.clone().into()),
            timeout_ms: Endpoint::DEFAULT_TIMEOUT,
            test_token: None,
        };

        match stats_utils::send_stats_payload(
            serialized_stats_payload,
            &endpoint,
            &self.config.api_key,
        )
        .await
        {
            Ok(()) => debug!("Successfully flushed stats"),
            Err(e) => {
                error!("Error sending stats: {e:?}");
            }
        }
    }
}
