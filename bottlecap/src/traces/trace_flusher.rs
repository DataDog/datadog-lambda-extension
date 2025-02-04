// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::str::FromStr;
use std::sync::Arc;
use datadog_trace_protobuf::pb::ClientStatsPayload;
use datadog_trace_utils::config_utils::trace_stats_url;
use datadog_trace_utils::stats_utils;
use tokio::sync::Mutex;
use tracing::{debug, error};

use crate::config::Config;
use crate::traces::trace_aggregator::{BatchAggregator, BatchData};
use datadog_trace_utils::trace_utils::{self, SendData};
use ddcommon::Endpoint;

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessBatchFlusher {
    pub aggregator: Arc<Mutex<BatchAggregator>>,
    pub config: Arc<Config>,
    endpoint: Endpoint,
}

impl ServerlessBatchFlusher {
    pub fn new(aggregator: Arc<Mutex<BatchAggregator>>, config: Arc<Config>) -> Self {
        let stats_url = trace_stats_url(&config.site);

        let endpoint = Endpoint {
            url: hyper::Uri::from_str(&stats_url).expect("can't make URI from stats url, exiting"),
            api_key: Some(config.api_key.clone().into()),
            timeout_ms: Endpoint::DEFAULT_TIMEOUT,
            test_token: None,
        };
        ServerlessBatchFlusher { aggregator, config, endpoint }
    }

    pub async fn flush(&self) {
        let mut guard = self.aggregator.lock().await;

        let mut traces = guard.get_batch();
        while !traces.is_empty() {
            self.send(traces).await;

            traces = guard.get_batch();
        }
    }

    async fn send(&self, b: Vec<BatchData>) {
        match &b[0] {

            BatchData::SD(_) => {
                let traces: Vec<SendData> = b.iter().filter_map(|t| if let BatchData::SD(sd) = t { Some(sd.clone()) } else { None }).collect();
                debug!("Flushing {} traces", traces.len());

                for traces in trace_utils::coalesce_send_data(traces) {
                    match traces
                        .send_proxy(self.config.https_proxy.as_deref())
                        .await
                        .last_result
                    {
                        Ok(_) => debug!("Successfully flushed traces"),
                        Err(e) => {
                            error!("Error sending trace: {e:?}");
                            // TODO: Retries
                        }
                    }
                }
            }
            BatchData::CSP(_) => {
                let stats: Vec<ClientStatsPayload> = b.iter().filter_map(|t| if let BatchData::CSP(sd) = t { Some(sd.clone()) } else { None }).collect();
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

                match stats_utils::send_stats_payload(
                    serialized_stats_payload,
                    &self.endpoint,
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
    }
}
