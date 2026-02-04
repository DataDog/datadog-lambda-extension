// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use dogstatsd::api_key::ApiKeyFactory;
use libdd_common::Endpoint;
use libdd_trace_utils::{
    config_utils::trace_intake_url_prefixed,
    send_data::SendDataBuilder,
    trace_utils::{self, SendData},
};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio::task::JoinSet;
use tracing::{debug, error};

use crate::config::Config;
use crate::lifecycle::invocation::processor::S_TO_MS;
use crate::traces::hyper_client::{self, HyperClient};
use crate::traces::trace_aggregator_service::AggregatorHandle;

pub struct TraceFlusher {
    pub aggregator_handle: AggregatorHandle,
    pub config: Arc<Config>,
    pub api_key_factory: Arc<ApiKeyFactory>,
    pub additional_endpoints: Vec<Endpoint>,
    /// Cached HTTP client, lazily initialized on first use.
    http_client: OnceCell<HyperClient>,
}

impl TraceFlusher {
    #[must_use]
    pub fn new(
        aggregator_handle: AggregatorHandle,
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

        TraceFlusher {
            aggregator_handle,
            config,
            api_key_factory,
            additional_endpoints,
            http_client: OnceCell::new(),
        }
    }

    /// Flushes traces by getting every available batch on the aggregator.
    /// If `failed_traces` is provided, it will attempt to send those instead of fetching new traces.
    /// Returns any traces that failed to send and should be retried.
    pub async fn flush(&self, failed_traces: Option<Vec<SendData>>) -> Option<Vec<SendData>> {
        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!(
                "TRACES | Failed to resolve API key, dropping aggregated data and skipping flushing."
            );
            if let Err(e) = self.aggregator_handle.clear() {
                error!("TRACES | Failed to clear aggregator data: {e}");
            }
            return None;
        };

        // Get or create the cached HTTP client
        let Some(http_client) = self.get_or_init_http_client().await else {
            error!("TRACES | Failed to create HTTP client, skipping flush");
            return None;
        };

        let mut failed_batch: Vec<SendData> = Vec::new();

        if let Some(traces) = failed_traces {
            // If we have traces from a previous failed attempt, try to send those first
            if !traces.is_empty() {
                debug!(
                    "TRACES | Retrying to send {} previously failed batches",
                    traces.len()
                );
                let retry_result = Self::send_traces(traces, None, http_client.clone()).await;
                if retry_result.is_some() {
                    // Still failed, return to retry later
                    return retry_result;
                }
            }
        }

        let all_batches = match self.aggregator_handle.get_batches().await {
            Ok(v) => v,
            Err(e) => {
                error!("TRACES | Failed to fetch batches from aggregator service: {e}");
                return None;
            }
        };

        let mut batch_tasks = JoinSet::new();

        for trace_builders in all_batches {
            let traces: Vec<_> = trace_builders
                .into_iter()
                .map(|builder| builder.with_api_key(api_key.as_str()))
                .map(SendDataBuilder::build)
                .collect();

            let traces_clone = traces.clone();
            let client_clone = http_client.clone();
            batch_tasks
                .spawn(async move { Self::send_traces(traces_clone, None, client_clone).await });

            for endpoint in self.additional_endpoints.clone() {
                let traces_clone = traces.clone();
                let client_clone = http_client.clone();
                batch_tasks.spawn(async move {
                    Self::send_traces(traces_clone, Some(endpoint), client_clone).await
                });
            }
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

    /// Returns a clone of the cached HTTP client, initializing it if necessary.
    ///
    /// The client is created once and reused for all subsequent flushes,
    /// providing connection pooling and TLS session reuse.
    ///
    /// Returns `None` if client creation fails. The error is logged but not cached,
    /// allowing retry on subsequent calls.
    async fn get_or_init_http_client(&self) -> Option<HyperClient> {
        match self
            .http_client
            .get_or_try_init(|| async {
                hyper_client::create_client(
                    self.config.proxy_https.as_ref(),
                    self.config.tls_cert_file.as_ref(),
                )
            })
            .await
        {
            Ok(client) => Some(client.clone()),
            Err(e) => {
                error!("TRACES | Failed to create HTTP client: {e}");
                None
            }
        }
    }

    /// Sends traces to the Datadog intake endpoint using the provided HTTP client.
    ///
    /// # Arguments
    ///
    /// * `traces` - The traces to send
    /// * `override_endpoint` - If `Some`, sends to this endpoint instead of the trace's
    ///   configured endpoint. Used for sending to additional endpoints.
    /// * `http_client` - The HTTP client to use for sending
    ///
    /// # Returns
    ///
    /// Returns the traces back if there was an error sending them (for retry).
    async fn send_traces(
        traces: Vec<SendData>,
        override_endpoint: Option<Endpoint>,
        http_client: HyperClient,
    ) -> Option<Vec<SendData>> {
        if traces.is_empty() {
            return None;
        }
        let start = tokio::time::Instant::now();
        let coalesced_traces = trace_utils::coalesce_send_data(traces);
        tokio::task::yield_now().await;
        debug!("TRACES | Flushing {} traces", coalesced_traces.len());

        for trace in &coalesced_traces {
            let trace_to_send = match &override_endpoint {
                Some(endpoint) => trace.with_endpoint(endpoint.clone()),
                None => trace.clone(),
            };

            let send_result = trace_to_send.send(&http_client).await.last_result;

            if let Err(e) = send_result {
                error!("TRACES | Request failed: {e:?}");
                // Return the original traces for retry
                return Some(coalesced_traces.clone());
            }
        }

        debug!("TRACES | Flushing took {} ms", start.elapsed().as_millis());
        None
    }
}
