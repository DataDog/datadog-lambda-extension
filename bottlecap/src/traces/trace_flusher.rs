// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use ddcommon::Endpoint;
use futures::future::join_all;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error};

use datadog_trace_utils::{
    config_utils::trace_intake_url_prefixed,
    send_data::SendDataBuilder,
    trace_utils::{self, SendData},
};
use dogstatsd::api_key::ApiKeyFactory;

use crate::config::Config;
use crate::lifecycle::invocation::processor::S_TO_MS;
use crate::traces::trace_aggregator::TraceAggregator;

#[async_trait]
pub trait TraceFlusher {
    fn new(
        aggregator: Arc<Mutex<TraceAggregator>>,
        config: Arc<Config>,
        api_key_factory: Arc<ApiKeyFactory>,
    ) -> Self
    where
        Self: Sized;
    /// Given a `Vec<SendData>`, a tracer payload, send it to the Datadog intake endpoint.
    /// Returns the traces back if there was an error sending them.
    async fn send(
        &self,
        traces: Vec<SendData>,
        endpoint: Option<&Endpoint>,
    ) -> Option<Vec<SendData>>;

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
    pub api_key_factory: Arc<ApiKeyFactory>,
    pub additional_endpoints: Vec<Endpoint>,
}

#[async_trait]
impl TraceFlusher for ServerlessTraceFlusher {
    fn new(
        aggregator: Arc<Mutex<TraceAggregator>>,
        config: Arc<Config>,
        api_key_factory: Arc<ApiKeyFactory>,
    ) -> Self {
        let mut additional_endpoints: Vec<Endpoint> = Vec::new();

        for (endpoint_url, api_keys) in config.apm_additional_endpoints.clone() {
            for api_key in api_keys {
                let trace_intake_url = trace_intake_url_prefixed(&endpoint_url);
                let endpoint = Endpoint {
                    url: hyper::Uri::from_str(&trace_intake_url)
                        .expect("can't parse additional trace intake URL, exiting"),
                    api_key: Some(api_key.clone().into()),
                    timeout_ms: config.flush_timeout * S_TO_MS,
                    test_token: None,
                };

                additional_endpoints.push(endpoint);
            }
        }

        ServerlessTraceFlusher {
            aggregator,
            config,
            api_key_factory,
            additional_endpoints,
        }
    }

    async fn flush(&self, failed_traces: Option<Vec<SendData>>) -> Option<Vec<SendData>> {
        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!("Purging the aggregated data and skipping flushing traces: Failed to resolve API key");
            {
                let mut guard = self.aggregator.lock().await;
                guard.clear()
            }
            return None;
        };

        let mut failed_batch: Vec<SendData> = Vec::new();

        if let Some(traces) = failed_traces {
            // If we have traces from a previous failed attempt, try to send those first
            if !traces.is_empty() {
                debug!("Retrying to send {} previously failed traces", traces.len());
                let retry_result = self.send(traces, None).await;
                if retry_result.is_some() {
                    // Still failed, return to retry later
                    return retry_result;
                }
            }
        }

        // Process new traces from the aggregator
        let mut guard = self.aggregator.lock().await;
        let mut trace_builders = guard.get_batch();

        while !trace_builders.is_empty() {
            let traces: Vec<_> = trace_builders
                .into_iter()
                // Lazily set the API key
                .map(|builder| builder.with_api_key(api_key))
                .map(SendDataBuilder::build)
                .collect();
            if let Some(mut failed) = self.send(traces.clone(), None).await {
                failed_batch.append(&mut failed);
            }

            // Send to additional endpoints
            let tasks = self.additional_endpoints.iter().map(|endpoint| {
                let traces_clone = traces.clone();
                async move { self.send(traces_clone, Some(endpoint)).await }
            });
            for mut failed in join_all(tasks).await.into_iter().flatten() {
                failed_batch.append(&mut failed);
            }

            // Stop processing more batches if we have a failure
            if !failed_batch.is_empty() {
                break;
            }

            trace_builders = guard.get_batch();
        }

        if !failed_batch.is_empty() {
            return Some(failed_batch);
        }

        None
    }

    async fn send(
        &self,
        traces: Vec<SendData>,
        endpoint: Option<&Endpoint>,
    ) -> Option<Vec<SendData>> {
        if traces.is_empty() {
            return None;
        }
        let start = std::time::Instant::now();

        let coalesced_traces = trace_utils::coalesce_send_data(traces);
        let mut tasks = Vec::with_capacity(coalesced_traces.len());
        debug!("Flushing {} traces", coalesced_traces.len());

        for trace in &coalesced_traces {
            let trace_with_endpoint = match endpoint {
                Some(additional_endpoint) => trace.with_endpoint(additional_endpoint.clone()),
                None => trace.clone(),
            };
            let proxy = self.config.proxy_https.clone();
            tasks.push(tokio::spawn(async move {
                trace_with_endpoint
                    .send_proxy(proxy.as_deref())
                    .await
                    .last_result
            }));
        }

        for task in tasks {
            match task.await {
                Ok(result) => {
                    if let Err(e) = result {
                        error!("Error sending trace: {e:?}");
                        // Return the original traces for retry
                        return Some(coalesced_traces.clone());
                    }
                }
                Err(e) => {
                    error!("Task join error: {e:?}");
                    // Return the original traces for retry if a task panics
                    return Some(coalesced_traces.clone());
                }
            }
        }
        debug!("Flushing traces took {}ms", start.elapsed().as_millis());
        None
    }
}
