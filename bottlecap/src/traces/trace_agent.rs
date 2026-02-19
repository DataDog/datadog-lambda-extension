// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, post},
};
use serde_json::json;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{
    Mutex,
    mpsc::{self, Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tower_http::limit::RequestBodyLimitLayer;
use tracing::{debug, error, warn};

use crate::traces::trace_processor::SendingTraceProcessor;
use crate::{
    appsec::processor::Processor as AppSecProcessor,
    config,
    http::{extract_request_body, handler_not_found},
    lifecycle::invocation::{
        context::ReparentingInfo, processor_service::InvocationProcessorHandle,
    },
    tags::provider,
    traces::{
        INVOCATION_SPAN_RESOURCE,
        proxy_aggregator::{self, ProxyRequest},
        span_dedup::DedupKey,
        span_dedup_service::DedupHandle,
        stats_aggregator,
        stats_generator::StatsGenerator,
        stats_processor,
        trace_aggregator::SendDataBuilderInfo,
        trace_aggregator_service::AggregatorHandle,
        trace_processor,
    },
};
use libdd_common::hyper_migration;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::trace_utils::{self};

use crate::traces::stats_concentrator_service::StatsConcentratorHandle;

const TRACE_AGENT_PORT: usize = 8126;

// Agent endpoints
const V4_TRACE_ENDPOINT_PATH: &str = "/v0.4/traces";
const V5_TRACE_ENDPOINT_PATH: &str = "/v0.5/traces";
const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";
const DSM_AGENT_PATH: &str = "/v0.1/pipeline_stats";
const PROFILING_ENDPOINT_PATH: &str = "/profiling/v1/input";
const LLM_OBS_EVAL_METRIC_ENDPOINT_PATH: &str = "/evp_proxy/v2/api/intake/llm-obs/v1/eval-metric";
const LLM_OBS_EVAL_METRIC_ENDPOINT_PATH_V2: &str =
    "/evp_proxy/v2/api/intake/llm-obs/v2/eval-metric";
const LLM_OBS_SPANS_ENDPOINT_PATH: &str = "/evp_proxy/v2/api/v2/llmobs";
const INFO_ENDPOINT_PATH: &str = "/info";
const V1_DEBUGGER_ENDPOINT_PATH: &str = "/debugger/v1/input";
const V2_DEBUGGER_ENDPOINT_PATH: &str = "/debugger/v2/input";
const DEBUGGER_DIAGNOSTICS_ENDPOINT_PATH: &str = "/debugger/v1/diagnostics";
const INSTRUMENTATION_ENDPOINT_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

// Intake endpoints
const DSM_INTAKE_PATH: &str = "/api/v0.1/pipeline_stats";
const LLM_OBS_SPANS_INTAKE_PATH: &str = "/api/v2/llmobs";
const LLM_OBS_EVAL_METRIC_INTAKE_PATH: &str = "/api/intake/llm-obs/v1/eval-metric";
const LLM_OBS_EVAL_METRIC_INTAKE_PATH_V2: &str = "/api/intake/llm-obs/v2/eval-metric";
const PROFILING_INTAKE_PATH: &str = "/api/v2/profile";
const V1_DEBUGGER_LOGS_INTAKE_PATH: &str = "/api/v2/logs";
const V2_DEBUGGER_INTAKE_PATH: &str = "/api/v2/debugger";
const INSTRUMENTATION_INTAKE_PATH: &str = "/api/v2/apmtelemetry";

const TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;
const STATS_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;
pub const TRACE_REQUEST_BODY_LIMIT: usize = 50 * 1024 * 1024;
pub const DEFAULT_REQUEST_BODY_LIMIT: usize = 2 * 1024 * 1024;
pub const MAX_CONTENT_LENGTH: usize = 50 * 1024 * 1024;
const LAMBDA_LOAD_SPAN: &str = "aws.lambda.load";

#[derive(Clone)]
pub struct TraceState {
    pub config: Arc<config::Config>,
    pub trace_sender: Arc<trace_processor::SendingTraceProcessor>,
    pub invocation_processor_handle: InvocationProcessorHandle,
    pub tags_provider: Arc<provider::Provider>,
    pub span_deduper: DedupHandle,
}

#[derive(Clone)]
pub struct StatsState {
    pub stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
    pub stats_tx: Sender<pb::ClientStatsPayload>,
}

#[derive(Clone)]
pub struct ProxyState {
    pub config: Arc<config::Config>,
    pub proxy_aggregator: Arc<Mutex<proxy_aggregator::Aggregator>>,
}

pub struct TraceAgent {
    pub config: Arc<config::Config>,
    pub trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    pub stats_aggregator: Arc<Mutex<stats_aggregator::StatsAggregator>>,
    pub stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
    pub proxy_aggregator: Arc<Mutex<proxy_aggregator::Aggregator>>,
    pub tags_provider: Arc<provider::Provider>,
    invocation_processor_handle: InvocationProcessorHandle,
    appsec_processor: Option<Arc<Mutex<AppSecProcessor>>>,
    shutdown_token: CancellationToken,
    tx: Sender<SendDataBuilderInfo>,
    stats_concentrator: StatsConcentratorHandle,
    span_deduper: DedupHandle,
}

#[derive(Clone, Copy)]
pub enum ApiVersion {
    V04,
    V05,
}

impl TraceAgent {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<config::Config>,
        aggregator_handle: AggregatorHandle,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        stats_aggregator: Arc<Mutex<stats_aggregator::StatsAggregator>>,
        stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
        proxy_aggregator: Arc<Mutex<proxy_aggregator::Aggregator>>,
        invocation_processor_handle: InvocationProcessorHandle,
        appsec_processor: Option<Arc<Mutex<AppSecProcessor>>>,
        tags_provider: Arc<provider::Provider>,
        stats_concentrator: StatsConcentratorHandle,
        span_deduper: DedupHandle,
    ) -> TraceAgent {
        // Set up a channel to send processed traces to our trace aggregator. tx is passed through each
        // endpoint_handler to the trace processor, which uses it to send de-serialized
        // processed trace payloads to our trace aggregator.
        let (trace_tx, mut trace_rx): (Sender<SendDataBuilderInfo>, Receiver<SendDataBuilderInfo>) =
            mpsc::channel(TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE);

        // Start the trace aggregator, which receives and buffers trace payloads to be consumed by the trace flusher.
        tokio::spawn(async move {
            while let Some(tracer_payload_info) = trace_rx.recv().await {
                if let Err(e) = aggregator_handle.insert_payload(tracer_payload_info) {
                    error!("TRACE_AGENT | Failed to insert payload into aggregator: {e}");
                }
            }
        });

        TraceAgent {
            config: config.clone(),
            trace_processor,
            stats_aggregator,
            stats_processor,
            proxy_aggregator,
            invocation_processor_handle,
            appsec_processor,
            tags_provider,
            tx: trace_tx,
            shutdown_token: CancellationToken::new(),
            stats_concentrator,
            span_deduper,
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let now = Instant::now();

        // Set up a channel to send processed stats to our stats aggregator.
        let (stats_tx, mut stats_rx): (
            Sender<pb::ClientStatsPayload>,
            Receiver<pb::ClientStatsPayload>,
        ) = mpsc::channel(STATS_PAYLOAD_CHANNEL_BUFFER_SIZE);

        // Start the stats aggregator, which receives and buffers stats payloads to be consumed by the stats flusher.
        let stats_aggregator = self.stats_aggregator.clone();
        tokio::spawn(async move {
            while let Some(stats_payload) = stats_rx.recv().await {
                let mut aggregator = stats_aggregator.lock().await;
                aggregator.add(stats_payload);
            }
        });

        let router = self.make_router(stats_tx);

        let port = u16::try_from(TRACE_AGENT_PORT).expect("TRACE_AGENT_PORT is too large");
        let socket = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(&socket).await?;

        debug!("TRACE AGENT | Listening on port {TRACE_AGENT_PORT}");
        debug!(
            "TRACE AGENT | Time taken to start: {} ms",
            now.elapsed().as_millis()
        );

        let shutdown_token_clone = self.shutdown_token.clone();
        axum::serve(listener, router)
            .with_graceful_shutdown(Self::graceful_shutdown(shutdown_token_clone))
            .await?;

        Ok(())
    }

    fn make_router(&self, stats_tx: Sender<pb::ClientStatsPayload>) -> Router {
        let stats_generator = Arc::new(StatsGenerator::new(self.stats_concentrator.clone()));
        let trace_state = TraceState {
            config: Arc::clone(&self.config),
            trace_sender: Arc::new(SendingTraceProcessor {
                appsec: self.appsec_processor.clone(),
                processor: Arc::clone(&self.trace_processor),
                trace_tx: self.tx.clone(),
                stats_generator,
            }),
            invocation_processor_handle: self.invocation_processor_handle.clone(),
            tags_provider: Arc::clone(&self.tags_provider),
            span_deduper: self.span_deduper.clone(),
        };

        let stats_state = StatsState {
            stats_processor: Arc::clone(&self.stats_processor),
            stats_tx,
        };

        let proxy_state = ProxyState {
            config: Arc::clone(&self.config),
            proxy_aggregator: Arc::clone(&self.proxy_aggregator),
        };

        let trace_router = Router::new()
            .route(
                V4_TRACE_ENDPOINT_PATH,
                post(Self::v04_traces).put(Self::v04_traces),
            )
            .route(
                V5_TRACE_ENDPOINT_PATH,
                post(Self::v05_traces).put(Self::v05_traces),
            )
            .layer(RequestBodyLimitLayer::new(TRACE_REQUEST_BODY_LIMIT))
            .with_state(trace_state);

        let stats_router = Router::new()
            .route(STATS_ENDPOINT_PATH, post(Self::stats).put(Self::stats))
            .layer(RequestBodyLimitLayer::new(DEFAULT_REQUEST_BODY_LIMIT))
            .with_state(stats_state);

        let proxy_router = Router::new()
            .route(DSM_AGENT_PATH, post(Self::dsm_proxy))
            .route(PROFILING_ENDPOINT_PATH, post(Self::profiling_proxy))
            .route(
                LLM_OBS_EVAL_METRIC_ENDPOINT_PATH,
                post(Self::llm_obs_eval_metric_proxy),
            )
            .route(
                LLM_OBS_EVAL_METRIC_ENDPOINT_PATH_V2,
                post(Self::llm_obs_eval_metric_proxy_v2),
            )
            .route(LLM_OBS_SPANS_ENDPOINT_PATH, post(Self::llm_obs_spans_proxy))
            .route(V1_DEBUGGER_ENDPOINT_PATH, post(Self::debugger_logs_proxy))
            .route(V2_DEBUGGER_ENDPOINT_PATH, post(Self::debugger_intake_proxy))
            .route(
                DEBUGGER_DIAGNOSTICS_ENDPOINT_PATH,
                post(Self::debugger_intake_proxy),
            )
            .route(
                INSTRUMENTATION_ENDPOINT_PATH,
                post(Self::instrumentation_proxy),
            )
            .layer(RequestBodyLimitLayer::new(DEFAULT_REQUEST_BODY_LIMIT))
            .with_state(proxy_state);

        let info_router = Router::new().route(INFO_ENDPOINT_PATH, any(Self::info));

        Router::new()
            .merge(trace_router)
            .merge(stats_router)
            .merge(proxy_router)
            .merge(info_router)
            .fallback(handler_not_found)
            // Disable the default body limit so we can use our own limit
            .layer(DefaultBodyLimit::disable())
    }

    async fn graceful_shutdown(shutdown_token: CancellationToken) {
        shutdown_token.cancelled().await;
        debug!("TRACE_AGENT | Shutdown signal received, shutting down");
    }

    async fn v04_traces(State(state): State<TraceState>, request: Request) -> Response {
        Self::handle_traces(
            state.config,
            request,
            state.trace_sender,
            state.invocation_processor_handle,
            state.tags_provider,
            state.span_deduper,
            ApiVersion::V04,
        )
        .await
    }

    async fn v05_traces(State(state): State<TraceState>, request: Request) -> Response {
        Self::handle_traces(
            state.config,
            request,
            state.trace_sender,
            state.invocation_processor_handle,
            state.tags_provider,
            state.span_deduper,
            ApiVersion::V05,
        )
        .await
    }

    async fn stats(State(state): State<StatsState>, request: Request) -> Response {
        match state
            .stats_processor
            .process_stats(request, state.stats_tx)
            .await
        {
            Ok(result) => result.into_response(),
            Err(err) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error processing trace stats: {err}"),
            ),
        }
    }

    async fn dsm_proxy(State(state): State<ProxyState>, request: Request) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "trace.agent",
            DSM_INTAKE_PATH,
            "DSM",
        )
        .await
    }

    async fn profiling_proxy(State(state): State<ProxyState>, request: Request) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "intake.profile",
            PROFILING_INTAKE_PATH,
            "profiling",
        )
        .await
    }

    async fn llm_obs_eval_metric_proxy(
        State(state): State<ProxyState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "api",
            LLM_OBS_EVAL_METRIC_INTAKE_PATH,
            "llm_obs_eval_metric",
        )
        .await
    }

    async fn llm_obs_eval_metric_proxy_v2(
        State(state): State<ProxyState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "api",
            LLM_OBS_EVAL_METRIC_INTAKE_PATH_V2,
            "llm_obs_eval_metric",
        )
        .await
    }

    async fn llm_obs_spans_proxy(State(state): State<ProxyState>, request: Request) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "llmobs-intake",
            LLM_OBS_SPANS_INTAKE_PATH,
            "llm_obs_spans",
        )
        .await
    }

    // Used for `/debugger/v1/input` in Exception Replay
    async fn debugger_logs_proxy(State(state): State<ProxyState>, request: Request) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "http-intake.logs",
            V1_DEBUGGER_LOGS_INTAKE_PATH,
            "debugger_logs",
        )
        .await
    }

    // Used for `/debugger/v1/diagnostics` and `/debugger/v2/input` in Exception Replay
    async fn debugger_intake_proxy(State(state): State<ProxyState>, request: Request) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "debugger-intake",
            V2_DEBUGGER_INTAKE_PATH,
            "debugger",
        )
        .await
    }

    async fn instrumentation_proxy(State(state): State<ProxyState>, request: Request) -> Response {
        Self::handle_proxy(
            state.config,
            state.proxy_aggregator,
            request,
            "instrumentation-telemetry-intake",
            INSTRUMENTATION_INTAKE_PATH,
            "instrumentation",
        )
        .await
    }

    #[allow(clippy::unused_async)]
    async fn info() -> Response {
        let response_json = json!(
            {
                "endpoints": [
                    V4_TRACE_ENDPOINT_PATH,
                    V5_TRACE_ENDPOINT_PATH,
                    STATS_ENDPOINT_PATH,
                    DSM_AGENT_PATH,
                    PROFILING_ENDPOINT_PATH,
                    INFO_ENDPOINT_PATH,
                    LLM_OBS_EVAL_METRIC_ENDPOINT_PATH,
                    LLM_OBS_EVAL_METRIC_ENDPOINT_PATH_V2,
                    LLM_OBS_SPANS_ENDPOINT_PATH,
                    V1_DEBUGGER_ENDPOINT_PATH,
                    V2_DEBUGGER_ENDPOINT_PATH,
                    DEBUGGER_DIAGNOSTICS_ENDPOINT_PATH,
                ],
                "client_drop_p0s": true,
            }
        );
        (StatusCode::OK, response_json.to_string()).into_response()
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    async fn handle_traces(
        config: Arc<config::Config>,
        request: Request,
        trace_sender: Arc<SendingTraceProcessor>,
        invocation_processor_handle: InvocationProcessorHandle,
        tags_provider: Arc<provider::Provider>,
        deduper: DedupHandle,
        version: ApiVersion,
    ) -> Response {
        let start = Instant::now();
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("TRACE_AGENT | handle_traces | Error extracting request body: {e}"),
                );
            }
        };

        if let Some(content_length) = parts
            .headers
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.parse::<usize>().ok())
            .filter(|l| *l > MAX_CONTENT_LENGTH)
        {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Content-Length {content_length} exceeds maximum allowed size {MAX_CONTENT_LENGTH}"
                ),
            );
        }

        let tracer_header_tags = (&parts.headers).into();

        let (body_size, mut traces) = match version {
            ApiVersion::V04 => match trace_utils::get_traces_from_request_body(
                hyper_migration::Body::from_bytes(body),
            )
            .await
            {
                Ok(result) => result,
                Err(err) => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Error deserializing trace from request body: {err}"),
                    );
                }
            },
            ApiVersion::V05 => match trace_utils::get_v05_traces_from_request_body(
                hyper_migration::Body::from_bytes(body),
            )
            .await
            {
                Ok(result) => result,
                Err(err) => {
                    return error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Error deserializing trace from request body: {err}"),
                    );
                }
            },
        };

        let mut reparenting_info = match invocation_processor_handle.get_reparenting_info().await {
            Ok(info) => info,
            Err(e) => {
                error!("Failed to get reparenting info: {e}");
                VecDeque::new()
            }
        };

        for chunk in &mut traces {
            let original_chunk = std::mem::take(chunk);
            for mut span in original_chunk {
                // Check for duplicates
                let key = DedupKey::new(span.trace_id, span.span_id);
                let should_keep = match deduper.check_and_add(key, config.span_dedup_timeout).await
                {
                    Ok(should_keep) => {
                        if !should_keep {
                            debug!(
                                "Dropping duplicate span with trace_id: {}, span_id: {}",
                                span.trace_id, span.span_id
                            );
                        }
                        should_keep
                    }
                    Err(e) => {
                        warn!("Failed to check span in deduper, keeping span: {e}");
                        true
                    }
                };

                if !should_keep {
                    continue;
                }

                // If the aws.lambda.load span is found, we're in Python or Node.
                // We need to update the trace ID of the cold start span, reparent the `aws.lambda.load`
                // span to the cold start span, and eventually send the cold start span.
                if span.name == LAMBDA_LOAD_SPAN {
                    match invocation_processor_handle
                        .set_cold_start_span_trace_id(span.trace_id)
                        .await
                    {
                        Ok(Some(cold_start_span_id)) => {
                            span.parent_id = cold_start_span_id;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            error!("Failed to set cold start span trace ID: {e}");
                        }
                    }
                }

                if span.resource == INVOCATION_SPAN_RESOURCE
                    && let Err(e) = invocation_processor_handle
                        .add_tracer_span(span.clone())
                        .await
                {
                    error!("Failed to add tracer span to processor: {}", e);
                }
                handle_reparenting(&mut reparenting_info, &mut span);

                // Keep the span
                chunk.push(span);
            }
        }

        // Remove empty chunks
        traces.retain(|chunk| !chunk.is_empty());

        match invocation_processor_handle
            .update_reparenting(reparenting_info)
            .await
        {
            Ok(contexts_to_send) => {
                for ctx_to_send in contexts_to_send {
                    debug!("Invocation span is now ready. Sending: {ctx_to_send:?}");
                    if let Err(e) = invocation_processor_handle
                        .send_ctx_spans(&tags_provider, &trace_sender, ctx_to_send)
                        .await
                    {
                        error!("Failed to send context spans to processor: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("Failed to update reparenting: {e}");
            }
        }

        if let Err(err) = trace_sender
            .send_processed_traces(
                config,
                tags_provider,
                tracer_header_tags,
                traces,
                body_size,
                None,
            )
            .await
        {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error sending traces to the trace aggregator: {err:?}"),
            );
        }

        debug!(
            "TRACE_AGENT | Processing traces took: {} ms",
            start.elapsed().as_millis()
        );

        success_response("Successfully buffered traces to be aggregated.")
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_proxy(
        config: Arc<config::Config>,
        proxy_aggregator: Arc<Mutex<proxy_aggregator::Aggregator>>,
        request: Request,
        backend_domain: &str,
        backend_path: &str,
        context: &str,
    ) -> Response {
        debug!("TRACE_AGENT | Proxied request for {context}");
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("TRACE_AGENT | handle_proxy | Error extracting request body: {e}"),
                );
            }
        };

        let target_url = format!("https://{}.{}{}", backend_domain, config.site, backend_path);
        let proxy_request = ProxyRequest {
            headers: parts.headers,
            body,
            target_url,
        };

        let mut proxy_aggregator = proxy_aggregator.lock().await;
        proxy_aggregator.add(proxy_request);

        (
            StatusCode::OK,
            format!("Acknowledged request for {context}"),
        )
            .into_response()
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<SendDataBuilderInfo> {
        self.tx.clone()
    }

    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }
}

fn handle_reparenting(reparenting_info: &mut VecDeque<ReparentingInfo>, span: &mut pb::Span) {
    for rep_info in reparenting_info {
        if rep_info.needs_trace_id {
            rep_info.guessed_trace_id = span.trace_id;
            rep_info.needs_trace_id = false;
            debug!(
                "Guessed trace ID: {} for reparenting {rep_info:?}",
                span.trace_id
            );
        }
        if span.trace_id == rep_info.guessed_trace_id
            && span.parent_id == rep_info.parent_id_to_reparent
        {
            debug!(
                "Reparenting span {} with parent id {}",
                span.span_id, rep_info.invocation_span_id
            );
            span.parent_id = rep_info.invocation_span_id;
        }
    }
}

fn error_response<E: std::fmt::Display>(status: StatusCode, error: E) -> Response {
    error!("{}", error);
    (status, error.to_string()).into_response()
}

fn success_response(message: &str) -> Response {
    debug!("{}", message);
    (StatusCode::OK, json!({"rate_by_service": {}}).to_string()).into_response()
}
