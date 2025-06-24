// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error};

use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_utils::trace_utils::{self, SendData};

use crate::config::Config;
use crate::tags::provider::Provider;
use crate::traces::trace_aggregator::TraceAggregator;
use crate::traces::trace_processor::ServerlessTraceProcessor;

#[async_trait]
pub trait TraceFlusher {
    fn new(
        aggregator: Arc<Mutex<TraceAggregator>>,
        tags_provider: Arc<Provider>,
        config: Arc<Config>,
    ) -> Self
    where
        Self: Sized;
    /// Given a `Vec<SendData>`, a tracer payload, send it to the Datadog intake endpoint.
    /// Returns the traces back if there was an error sending them.
    async fn send(&self, traces: Vec<SendData>) -> Option<Vec<SendData>>;

    /// Flushes traces by getting every available batch on the aggregator.
    /// If `failed_traces` is provided, it will attempt to send those instead of fetching new traces.
    /// Returns any traces that failed to send and should be retried.
    async fn flush(&self, failed_traces: Option<Vec<SendData>>) -> Option<Vec<SendData>>;
}

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceFlusher {
    pub aggregator: Arc<Mutex<TraceAggregator>>,
    pub config: Arc<Config>,
    pub tags_provider: Arc<Provider>,
}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    fn new(
        aggregator: Arc<Mutex<TraceAggregator>>,
        tags_provider: Arc<Provider>,
        config: Arc<Config>,
    ) -> Self {
        ServerlessTraceFlusher {
            aggregator,
            tags_provider,
            config,
        }
    }

    async fn flush(&self, failed_traces: Option<Vec<SendData>>) -> Option<Vec<SendData>> {
        let mut failed_batch: Option<Vec<SendData>> = None;

        if let Some(traces) = failed_traces {
            // If we have traces from a previous failed attempt, try to send those first
            if !traces.is_empty() {
                debug!("Retrying to send {} previously failed traces", traces.len());
                let retry_result = self.send(traces).await;
                if retry_result.is_some() {
                    // Still failed, return to retry later
                    return retry_result;
                }
            }
        }

        // Process new traces from the aggregator
        let mut guard = self.aggregator.lock().await;
        // Pass the trace processor and config to get_batch
        let trace_processor = ServerlessTraceProcessor {
            obfuscation_config: Arc::new(obfuscation_config::ObfuscationConfig::new().unwrap()),
            resolved_api_key: self.config.api_key.clone(),
        };
        let mut traces = guard.get_batch(&trace_processor, &self.config, &self.tags_provider);

        while !traces.is_empty() {
            if let Some(failed) = self.send(traces).await {
                // Keep track of the failed batch
                failed_batch = Some(failed);
                // Stop processing more batches if we have a failure
                break;
            }

            traces = guard.get_batch(&trace_processor, &self.config, &self.tags_provider);
            println!("AJ TRACES REMAINING ARE: {:?}", traces);
        }

        failed_batch
    }

    async fn send(&self, traces: Vec<SendData>) -> Option<Vec<SendData>> {
        if traces.is_empty() {
            return None;
        }
        let start = std::time::Instant::now();
        debug!("Flushing {} traces", traces.len());

        // Since we return the original traces on error, we need to clone them before coalescing
        let traces_clone = traces.clone();

        let coalesced_traces = trace_utils::coalesce_send_data(traces);
        let mut tasks = Vec::with_capacity(coalesced_traces.len());

        for traces in coalesced_traces {
            let https_proxy = self.config.https_proxy.clone();
            tasks.push(tokio::spawn(async move {
                traces.send_proxy(https_proxy.as_deref()).await.last_result
            }));
        }

        for task in tasks {
            match task.await {
                Ok(result) => {
                    if let Err(e) = result {
                        error!("Error sending trace: {e:?}");
                        // Return the original traces for retry
                        return Some(traces_clone);
                    }
                }
                Err(e) => {
                    error!("Task join error: {e:?}");
                    // Return the original traces for retry if a task panics
                    return Some(traces_clone);
                }
            }
        }
        debug!("Flushing traces took {}ms", start.elapsed().as_millis());
        None
    }
}
