// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use log::{error, info};
use std::sync::Arc;
use tokio::sync::{mpsc::Receiver, Mutex};

use datadog_trace_utils::trace_utils;
use datadog_trace_utils::trace_utils::SendData;


#[async_trait]
pub trait TraceFlusher {
    /// Starts a trace flusher that listens for trace payloads sent to the tokio mpsc Receiver,
    /// implementing flushing logic that calls flush_traces.
    async fn start_trace_flusher(&self, mut rx: Receiver<SendData>);
    /// Flushes traces to the Datadog trace intake.
    async fn flush_traces(&self, traces: Vec<SendData>);
}

#[derive(Clone)]
pub struct ServerlessTraceFlusher {
    pub buffer: Arc<Mutex<Vec<SendData>>>
}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    async fn start_trace_flusher(&self, mut rx: Receiver<SendData>) {
        let buffer: Arc<Mutex<Vec<SendData>>> = Arc::new(Mutex::new(Vec::new()));

        let buffer_producer = buffer.clone();
        let buffer_consumer = buffer.clone();

        tokio::spawn(async move {
            while let Some(tracer_payload) = rx.recv().await {
                let mut buffer = buffer_producer.lock().await;
                buffer.push(tracer_payload);
            }
        });

        // loop {
        //     tokio::time::sleep(time::Duration::from_secs(config.trace_flush_interval)).await;

        //     let mut buffer = buffer_consumer.lock().await;
        //     if !buffer.is_empty() {
        //         self.flush_traces(buffer.to_vec()).await;
        //         buffer.clear();
        //     }
        // }
    }

    async fn flush_traces(&self, traces: Vec<SendData>) {
        if traces.is_empty() {
            return;
        }
        info!("Flushing {} traces", traces.len());

        for traces in trace_utils::coalesce_send_data(traces) {
            match traces.send().await.last_result {
                Ok(_) => info!("Successfully flushed traces"),
                Err(e) => {
                    error!("Error sending trace: {e:?}")
                    // TODO: Retries
                }
            }
        }
    }
}
