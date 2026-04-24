// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use dogstatsd::api_key::ApiKeyFactory;
use libdd_trace_obfuscation::obfuscation_config;
use tokio::sync::{Mutex as TokioMutex, mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::appsec::processor::Processor as AppSecProcessor;
use crate::config::Config;
use crate::lifecycle::invocation::processor_service::InvocationProcessorHandle;
use crate::tags::provider::Provider as TagProvider;
use crate::traces::{
    http_client as trace_http_client, proxy_aggregator,
    proxy_flusher::Flusher as ProxyFlusher,
    span_dedup_service,
    stats_aggregator::StatsAggregator,
    stats_concentrator_service::{self, StatsConcentratorHandle},
    stats_flusher, stats_processor, trace_agent,
    trace_aggregator::SendDataBuilderInfo,
    trace_aggregator_service::{self, AggregatorHandle as TraceAggregatorHandle},
    trace_flusher, trace_processor,
};

/// Handles produced by [`build_trace_agent`] / [`start_trace_agent`]. Holds
/// the trace-channel sender, the per-domain flushers, the shutdown token, and
/// the aggregator/concentrator handles the caller needs to drive flushes and
/// shut the pipeline down.
pub struct TraceAgentPipeline {
    pub trace_tx: Sender<SendDataBuilderInfo>,
    pub trace_flusher: Arc<trace_flusher::TraceFlusher>,
    pub trace_processor: Arc<trace_processor::ServerlessTraceProcessor>,
    pub stats_flusher: Arc<stats_flusher::StatsFlusher>,
    pub proxy_flusher: Arc<ProxyFlusher>,
    pub shutdown_token: CancellationToken,
    pub stats_concentrator: StatsConcentratorHandle,
    pub trace_aggregator_handle: TraceAggregatorHandle,
}

/// Builds the full trace-processing pipeline (trace + stats + proxy
/// aggregators, services, flushers) and the [`trace_agent::TraceAgent`] that
/// owns the HTTP listener. Spawns the aggregator/concentrator/dedup services
/// onto the current tokio runtime, but does **not** spawn the `TraceAgent`
/// itself. The caller owns `trace_agent` and is responsible for spawning
/// `trace_agent.start()`, optionally after further configuring it (for
/// example, via [`trace_agent::TraceAgent::with_flushing_service`]).
///
/// Note: the aggregator/concentrator/dedup tasks spawned here have no
/// external shutdown signal; they run until their command channels are
/// dropped. Callers that abandon the returned `TraceAgent` without either
/// spawning it or dropping the pipeline handles will leak those background
/// tasks for the lifetime of the process.
///
/// Most callers want [`start_trace_agent`] instead, which handles the spawn.
pub fn build_trace_agent(
    config: &Arc<Config>,
    api_key_factory: &Arc<ApiKeyFactory>,
    tags_provider: &Arc<TagProvider>,
    invocation_processor_handle: InvocationProcessorHandle,
    appsec_processor: Option<Arc<TokioMutex<AppSecProcessor>>>,
    client: &reqwest::Client,
) -> (trace_agent::TraceAgent, TraceAgentPipeline) {
    // Build one shared hyper-based HTTP client for trace and stats flushing.
    // This client type is required by libdd_trace_utils for SendData::send().
    let trace_http_client = trace_http_client::create_client(
        config.proxy_https.as_ref(),
        config.tls_cert_file.as_ref(),
        config.skip_ssl_validation,
    )
    .expect("Failed to create trace HTTP client");

    // Stats
    let (stats_concentrator_service, stats_concentrator_handle) =
        stats_concentrator_service::StatsConcentratorService::new(Arc::clone(config));
    tokio::spawn(stats_concentrator_service.run());
    let stats_aggregator: Arc<TokioMutex<StatsAggregator>> = Arc::new(TokioMutex::new(
        StatsAggregator::new_with_concentrator(stats_concentrator_handle.clone()),
    ));
    let stats_flusher = Arc::new(stats_flusher::StatsFlusher::new(
        api_key_factory.clone(),
        stats_aggregator.clone(),
        Arc::clone(config),
        trace_http_client.clone(),
    ));

    let stats_processor = Arc::new(stats_processor::ServerlessStatsProcessor {});

    // Traces
    let (trace_aggregator_service, trace_aggregator_handle) =
        trace_aggregator_service::AggregatorService::default();
    tokio::spawn(trace_aggregator_service.run());

    let trace_flusher = Arc::new(trace_flusher::TraceFlusher::new(
        trace_aggregator_handle.clone(),
        config.clone(),
        api_key_factory.clone(),
        trace_http_client,
    ));

    let obfuscation_config = obfuscation_config::ObfuscationConfig {
        tag_replace_rules: config.apm_replace_tags.clone(),
        http_remove_path_digits: config.apm_config_obfuscation_http_remove_paths_with_digits,
        http_remove_query_string: config.apm_config_obfuscation_http_remove_query_string,
        obfuscate_memcached: false,
        obfuscation_redis_enabled: false,
        obfuscation_redis_remove_all_args: false,
    };

    let trace_processor = Arc::new(trace_processor::ServerlessTraceProcessor {
        obfuscation_config: Arc::new(obfuscation_config),
    });

    let (span_dedup_service, span_dedup_handle) = span_dedup_service::DedupService::new();
    tokio::spawn(span_dedup_service.run());

    // Proxy
    let proxy_aggregator = Arc::new(TokioMutex::new(proxy_aggregator::Aggregator::default()));
    let proxy_flusher = Arc::new(ProxyFlusher::new(
        api_key_factory.clone(),
        Arc::clone(&proxy_aggregator),
        Arc::clone(tags_provider),
        Arc::clone(config),
        client.clone(),
    ));

    let trace_agent = trace_agent::TraceAgent::new(
        Arc::clone(config),
        trace_aggregator_handle.clone(),
        trace_processor.clone(),
        stats_aggregator,
        stats_processor,
        proxy_aggregator,
        invocation_processor_handle,
        appsec_processor,
        Arc::clone(tags_provider),
        stats_concentrator_handle.clone(),
        span_dedup_handle,
    );
    let pipeline = TraceAgentPipeline {
        trace_tx: trace_agent.get_sender_copy(),
        trace_flusher,
        trace_processor,
        stats_flusher,
        proxy_flusher,
        shutdown_token: trace_agent.shutdown_token(),
        stats_concentrator: stats_concentrator_handle,
        trace_aggregator_handle,
    };

    (trace_agent, pipeline)
}

/// Builds the trace-processing pipeline with [`build_trace_agent`] and spawns
/// the [`trace_agent::TraceAgent`] HTTP listener onto the current tokio
/// runtime. Convenience entry point for callers that do not need to
/// further configure the `TraceAgent` before spawning it.
///
/// Callers that need to attach a
/// [`crate::flushing::FlushingService`](crate::flushing::FlushingService)
/// (or otherwise customize the `TraceAgent`) should use
/// [`build_trace_agent`] directly and spawn the returned `TraceAgent`
/// themselves after applying the extra configuration, e.g. via
/// [`trace_agent::TraceAgent::with_flushing_service`].
pub fn start_trace_agent(
    config: &Arc<Config>,
    api_key_factory: &Arc<ApiKeyFactory>,
    tags_provider: &Arc<TagProvider>,
    invocation_processor_handle: InvocationProcessorHandle,
    appsec_processor: Option<Arc<TokioMutex<AppSecProcessor>>>,
    client: &reqwest::Client,
) -> TraceAgentPipeline {
    let (trace_agent, pipeline) = build_trace_agent(
        config,
        api_key_factory,
        tags_provider,
        invocation_processor_handle,
        appsec_processor,
        client,
    );

    tokio::spawn(async move {
        if let Err(e) = trace_agent.start().await {
            error!("Error starting trace agent: {e:?}");
        }
    });

    pipeline
}
