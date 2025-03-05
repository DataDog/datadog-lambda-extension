// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error};

use datadog_trace_utils::trace_utils::{self, SendData};

use crate::config::Config;
use crate::traces::trace_aggregator::TraceAggregator;

#[async_trait]
pub trait TraceFlusher {
    fn new(aggregator: Arc<Mutex<TraceAggregator>>, config: Arc<Config>) -> Self
    where
        Self: Sized;
    /// Flushes traces to the Datadog trace intake.
    async fn send(&self, traces: Vec<SendData>);

    async fn flush(&self);
}

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceFlusher {
    pub aggregator: Arc<Mutex<TraceAggregator>>,
    pub config: Arc<Config>,
}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    fn new(aggregator: Arc<Mutex<TraceAggregator>>, config: Arc<Config>) -> Self {
        ServerlessTraceFlusher { aggregator, config }
    }

    async fn flush(&self) {
        let mut guard = self.aggregator.lock().await;

        let mut traces = guard.get_batch();
        while !traces.is_empty() {
            self.send(traces).await;

            traces = guard.get_batch();
        }
    }

    async fn send(&self, traces: Vec<SendData>) {
        if traces.is_empty() {
            return;
        }

        let start = std::time::Instant::now();
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
                }
            }
        }
        debug!("Flushing traces took {}ms", start.elapsed().as_millis());
    }
}
