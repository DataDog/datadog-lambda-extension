// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use ddcommon::Endpoint;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
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
        traces: Vec<SendData>,
        endpoint: Option<&Endpoint>,
        proxy_https: &Option<String>,
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
            error!(
                "Purging the aggregated data and skipping flushing traces: Failed to resolve API key"
            );
            {
                let mut guard = self.aggregator.lock().await;
                guard.clear();
            }
            return None;
        };

        let mut failed_batch: Vec<SendData> = Vec::new();

        if let Some(traces) = failed_traces {
            // If we have traces from a previous failed attempt, try to send those first
            if !traces.is_empty() {
                debug!("Retrying to send {} previously failed traces", traces.len());
                let retry_result = Self::send(traces, None, &self.config.proxy_https).await;
                if retry_result.is_some() {
                    // Still failed, return to retry later
                    return retry_result;
                }
            }
        }

        // Process new traces from the aggregator
        let mut guard = self.aggregator.lock().await;
        let mut trace_builders = guard.get_batch();

        // Collect all batches and their tasks
        let mut batch_tasks = JoinSet::new();

        while !trace_builders.is_empty() {
            let traces: Vec<_> = trace_builders
                .into_iter()
                // Lazily set the API key
                .map(|builder| builder.with_api_key(api_key))
                .map(SendDataBuilder::build)
                .collect();

            tokio::task::yield_now().await;
            let traces_clone = traces.clone();
            let proxy_https = self.config.proxy_https.clone();
            batch_tasks.spawn(async move {
                let res = Self::send(traces_clone, None, &proxy_https).await;
                res
            });

            for endpoint in self.additional_endpoints.clone() {
                let traces_clone = traces.clone();
                let proxy_https = self.config.proxy_https.clone();
                batch_tasks.spawn(async move {
                    Self::send(traces_clone, Some(&endpoint), &proxy_https).await
                });
            }

            trace_builders = guard.get_batch();
        }

        while let Some(result) = batch_tasks.join_next().await {
            if let Ok(Some(mut failed)) = result {
                failed_batch.append(&mut failed);
            }
        }

        if !failed_batch.is_empty() {
            return Some(failed_batch);
        }

        None
    }

    async fn send(
        traces: Vec<SendData>,
        endpoint: Option<&Endpoint>,
        proxy_https: &Option<String>,
    ) -> Option<Vec<SendData>> {
        if traces.is_empty() {
            return None;
        }
        let start = tokio::time::Instant::now();
        let coalesced_traces = trace_utils::coalesce_send_data(traces);
        tokio::task::yield_now().await;
        debug!("Flushing {} traces", coalesced_traces.len());

        for trace in &coalesced_traces {
            let trace_with_endpoint = match endpoint {
                Some(additional_endpoint) => trace.with_endpoint(additional_endpoint.clone()),
                None => trace.clone(),
            };

            let send_result = trace_with_endpoint
                .send_proxy(proxy_https.as_deref())
                .await
                .last_result;

            if let Err(e) = send_result {
                error!("Error sending trace: {e:?}");
                // Return the original traces for retry
                return Some(coalesced_traces.clone());
            }
        }

        debug!("Flushing traces took {}ms", start.elapsed().as_millis());
        None
    }
}
