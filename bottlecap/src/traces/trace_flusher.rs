// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc::Receiver, Mutex};
use tracing::{debug, error};

use datadog_trace_utils::trace_utils::{self, SendData};

use crate::config::Config;

#[async_trait]
pub trait TraceFlusher {
    /// Starts a trace flusher that listens for trace payloads sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_traces.
    async fn start_trace_flusher(&self, mut rx: Receiver<SendData>);
    /// Flushes traces to the Datadog trace intake.
    async fn send(&self, traces: Vec<SendData>);

    async fn flush(&self);
}

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceFlusher {
    pub buffer: Arc<Mutex<Vec<SendData>>>,
    pub config: Arc<Config>,
}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    async fn start_trace_flusher(&self, mut rx: Receiver<SendData>) {
        let buffer_producer = self.buffer.clone();
        tokio::spawn(async move {
            while let Some(tracer_payload) = rx.recv().await {
                let mut buffer = buffer_producer.lock().await;
                buffer.push(tracer_payload);
            }
        });
    }

    async fn flush(&self) {
        let mut buffer = self.buffer.lock().await;
        if !buffer.is_empty() {
            self.send(buffer.to_vec()).await;
            buffer.clear();
        }
    }

    async fn send(&self, traces: Vec<SendData>) {
        if traces.is_empty() {
            return;
        }
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
}
