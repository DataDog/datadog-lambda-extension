// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use dogstatsd::api_key::ApiKeyFactory;
use libdd_trace_obfuscation::obfuscation_config;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc::Sender;
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
    stats_concentrator_service, stats_flusher, stats_processor, trace_agent,
    trace_aggregator::SendDataBuilderInfo,
    trace_aggregator_service::{self, AggregatorHandle as TraceAggregatorHandle},
    trace_flusher, trace_processor,
};

pub use crate::traces::stats_concentrator_service::StatsConcentratorHandle;

/// Wires up the full trace-processing pipeline (trace + stats + proxy
/// aggregators, services, flushers, and the [`trace_agent::TraceAgent`] HTTP
/// listener) and spawns each background task onto the current tokio runtime.
/// Returns the handles callers need to drive flushing and cancel the
/// listener.
#[allow(clippy::type_complexity)]
pub fn start_trace_agent(
    config: &Arc<Config>,
    api_key_factory: &Arc<ApiKeyFactory>,
    tags_provider: &Arc<TagProvider>,
    invocation_processor_handle: InvocationProcessorHandle,
    appsec_processor: Option<Arc<TokioMutex<AppSecProcessor>>>,
    client: &reqwest::Client,
) -> (
    Sender<SendDataBuilderInfo>,
    Arc<trace_flusher::TraceFlusher>,
    Arc<trace_processor::ServerlessTraceProcessor>,
    Arc<stats_flusher::StatsFlusher>,
    Arc<ProxyFlusher>,
    CancellationToken,
    StatsConcentratorHandle,
    TraceAggregatorHandle,
) {
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
    let trace_agent_channel = trace_agent.get_sender_copy();
    let shutdown_token = trace_agent.shutdown_token();

    tokio::spawn(async move {
        if let Err(e) = trace_agent.start().await {
            error!("Error starting trace agent: {e:?}");
        }
    });

    (
        trace_agent_channel,
        trace_flusher,
        trace_processor,
        stats_flusher,
        proxy_flusher,
        shutdown_token,
        stats_concentrator_handle,
        trace_aggregator_handle,
    )
}
