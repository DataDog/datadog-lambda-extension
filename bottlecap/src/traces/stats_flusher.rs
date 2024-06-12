// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use log::{debug, error, info};
use std::{sync::Arc, time};
use tokio::sync::{mpsc::Receiver, Mutex};

use datadog_trace_protobuf::pb;
use datadog_trace_utils::stats_utils;

use crate::config::Config;

#[async_trait]
pub trait StatsFlusher {
    /// Starts a stats flusher that listens for stats payloads sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_stats.
    async fn start_stats_flusher(
        &self,
        config: Arc<Config>,
        mut rx: Receiver<pb::ClientStatsPayload>,
    );
    /// Flushes stats to the Datadog trace stats intake.
    async fn flush_stats(&self, config: Arc<Config>, traces: Vec<pb::ClientStatsPayload>);
}

#[derive(Clone)]
pub struct ServerlessStatsFlusher {}

#[async_trait]
impl StatsFlusher for ServerlessStatsFlusher {
    async fn start_stats_flusher(
        &self,
        config: Arc<Config>,
        mut rx: Receiver<pb::ClientStatsPayload>,
    ) {
        let buffer: Arc<Mutex<Vec<pb::ClientStatsPayload>>> = Arc::new(Mutex::new(Vec::new()));

        let buffer_producer = buffer.clone();
        let buffer_consumer = buffer.clone();

        tokio::spawn(async move {
            while let Some(stats_payload) = rx.recv().await {
                let mut buffer = buffer_producer.lock().await;
                buffer.push(stats_payload);
            }
        });

        loop {
            tokio::time::sleep(time::Duration::from_secs(config.stats_flush_interval)).await;

            let mut buffer = buffer_consumer.lock().await;
            if !buffer.is_empty() {
                self.flush_stats(config.clone(), buffer.to_vec()).await;
                buffer.clear();
            }
        }
    }

    async fn flush_stats(&self, config: Arc<Config>, stats: Vec<pb::ClientStatsPayload>) {
        if stats.is_empty() {
            return;
        }
        info!("Flushing {} stats", stats.len());

        let stats_payload = stats_utils::construct_stats_payload(stats);

        debug!("Stats payload to be sent: {stats_payload:?}");

        let serialized_stats_payload = match stats_utils::serialize_stats_payload(stats_payload) {
            Ok(res) => res,
            Err(err) => {
                error!("Failed to serialize stats payload, dropping stats: {err}");
                return;
            }
        };

        match stats_utils::send_stats_payload(
            serialized_stats_payload,
            &config.trace_stats_intake,
            config.trace_stats_intake.api_key.as_ref().unwrap(),
        )
        .await
        {
            Ok(_) => info!("Successfully flushed stats"),
            Err(e) => {
                error!("Error sending stats: {e:?}")
            }
        }
    }
}
