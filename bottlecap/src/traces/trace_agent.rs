// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, post},
    Router,
};
use reqwest;
use serde_json::json;
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::config;
use crate::http::{extract_request_body, get_client, handler_not_found};
use crate::lifecycle::invocation::context::ReparentingInfo;
use crate::lifecycle::invocation::processor::Processor as InvocationProcessor;
use crate::tags::provider;
use crate::traces::{
    stats_aggregator, stats_processor, trace_aggregator, trace_processor, INVOCATION_SPAN_RESOURCE,
};
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils::{self, SendData};
use ddcommon::hyper_migration;
use dogstatsd::api_key::ApiKeyFactory;

const TRACE_AGENT_PORT: usize = 8126;
const V4_TRACE_ENDPOINT_PATH: &str = "/v0.4/traces";
const V5_TRACE_ENDPOINT_PATH: &str = "/v0.5/traces";
const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";
const DSM_ENDPOINT_PATH: &str = "/api/v0.1/pipeline_stats";
const DSM_AGENT_PATH: &str = "/v0.1/pipeline_stats";
const PROFILING_ENDPOINT_PATH: &str = "/profiling/v1/input";
const PROFILING_BACKEND_PATH: &str = "/api/v2/profile";
const LLM_OBS_SPANS_INTAKE_PATH: &str = "/api/v2/llmobs";
const LLM_OBS_EVAL_METRIC_INTAKE_PATH: &str = "/api/intake/llm-obs/v1/eval-metric";
const LLM_OBS_EVAL_METRIC_INTAKE_PATH_V2: &str = "/api/intake/llm-obs/v2/eval-metric";
const LLM_OBS_EVAL_METRIC_ENDPOINT_PATH: &str = "/evp_proxy/v2/api/intake/llm-obs/v1/eval-metric";
const LLM_OBS_EVAL_METRIC_ENDPOINT_PATH_V2: &str =
    "/evp_proxy/v2/api/intake/llm-obs/v2/eval-metric";
const LLM_OBS_SPANS_ENDPOINT_PATH: &str = "/evp_proxy/v2/api/v2/llmobs";
const DD_ADDITIONAL_TAGS_HEADER: &str = "X-Datadog-Additional-Tags";
const INFO_ENDPOINT_PATH: &str = "/info";
const DEBUGGER_ENDPOINT_PATH: &str = "/debugger/v1/input";
const DEBUGGER_LOGS_INTAKE_PATH: &str = "/api/v2/logs";
const TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;
const STATS_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;
pub const MAX_CONTENT_LENGTH: usize = 10 * 1024 * 1024;
const LAMBDA_LOAD_SPAN: &str = "aws.lambda.load";

type AgentState = (
    Arc<config::Config>,
    Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    Sender<SendData>,
    Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
    Sender<pb::ClientStatsPayload>,
    Arc<Mutex<InvocationProcessor>>,
    Arc<provider::Provider>,
    reqwest::Client,
    Arc<ApiKeyFactory>,
);

pub struct TraceAgent {
    pub config: Arc<config::Config>,
    pub trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    pub stats_aggregator: Arc<Mutex<stats_aggregator::StatsAggregator>>,
    pub stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
    pub tags_provider: Arc<provider::Provider>,
    invocation_processor: Arc<Mutex<InvocationProcessor>>,
    http_client: reqwest::Client,
    api_key_factory: Arc<ApiKeyFactory>,
    tx: Sender<SendData>,
    shutdown_token: CancellationToken,
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
        trace_aggregator: Arc<Mutex<trace_aggregator::TraceAggregator>>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        stats_aggregator: Arc<Mutex<stats_aggregator::StatsAggregator>>,
        stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
        tags_provider: Arc<provider::Provider>,
        api_key_factory: Arc<ApiKeyFactory>,
    ) -> TraceAgent {
        // setup a channel to send processed traces to our flusher. tx is passed through each
        // endpoint_handler to the trace processor, which uses it to send de-serialized
        // processed trace payloads to our trace flusher.
        let (trace_tx, mut trace_rx): (Sender<SendData>, Receiver<SendData>) =
            mpsc::channel(TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE);

        // start our trace flusher. receives trace payloads and handles buffering + deciding when to
        // flush to backend.

        tokio::spawn(async move {
            while let Some(tracer_payload) = trace_rx.recv().await {
                let mut aggregator = trace_aggregator.lock().await;
                aggregator.add(tracer_payload);
            }
        });

        TraceAgent {
            config: config.clone(),
            trace_processor,
            stats_aggregator,
            stats_processor,
            invocation_processor,
            tags_provider,
            http_client: get_client(config),
            tx: trace_tx,
            api_key_factory,
            shutdown_token: CancellationToken::new(),
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let now = Instant::now();

        // channels to send processed stats to our stats flusher.
        let (stats_tx, mut stats_rx): (
            Sender<pb::ClientStatsPayload>,
            Receiver<pb::ClientStatsPayload>,
        ) = mpsc::channel(STATS_PAYLOAD_CHANNEL_BUFFER_SIZE);

        // Receive stats payload and send it to the aggregator
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

        debug!("Trace Agent started: listening on port {TRACE_AGENT_PORT}");
        debug!(
            "Time taken start the Trace Agent: {} ms",
            now.elapsed().as_millis()
        );

        let shutdown_token_clone = self.shutdown_token.clone();
        axum::serve(listener, router)
            .with_graceful_shutdown(Self::graceful_shutdown(shutdown_token_clone))
            .await?;

        Ok(())
    }

    fn make_router(&self, stats_tx: Sender<pb::ClientStatsPayload>) -> Router {
        let state: AgentState = (
            Arc::clone(&self.config),
            Arc::clone(&self.trace_processor),
            self.tx.clone(),
            Arc::clone(&self.stats_processor),
            stats_tx,
            Arc::clone(&self.invocation_processor),
            Arc::clone(&self.tags_provider),
            self.http_client.clone(),
            Arc::clone(&self.api_key_factory),
        );

        Router::new()
            .route(
                V4_TRACE_ENDPOINT_PATH,
                post(Self::v04_traces).put(Self::v04_traces),
            )
            .route(
                V5_TRACE_ENDPOINT_PATH,
                post(Self::v05_traces).put(Self::v05_traces),
            )
            .route(STATS_ENDPOINT_PATH, post(Self::stats).put(Self::stats))
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
            .route(DEBUGGER_ENDPOINT_PATH, post(Self::debugger_logs_proxy))
            .route(INFO_ENDPOINT_PATH, any(Self::info))
            .fallback(handler_not_found)
            .with_state(state)
    }

    async fn graceful_shutdown(shutdown_token: CancellationToken) {
        shutdown_token.cancelled().await;
        debug!("Trace Agent | Shutdown signal received, shutting down");
    }

    async fn v04_traces(
        State((config, trace_processor, trace_tx, _, _, invocation_processor, tags_provider, _, _)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_traces(
            config,
            request,
            trace_processor,
            trace_tx,
            invocation_processor,
            tags_provider,
            ApiVersion::V04,
        )
        .await
    }

    async fn v05_traces(
        State((config, trace_processor, trace_tx, _, _, invocation_processor, tags_provider, _, _)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_traces(
            config,
            request,
            trace_processor,
            trace_tx,
            invocation_processor,
            tags_provider,
            ApiVersion::V05,
        )
        .await
    }

    async fn stats(
        State((_, _, _, stats_processor, stats_tx, _, _, _, _)): State<AgentState>,
        request: Request,
    ) -> Response {
        match stats_processor.process_stats(request, stats_tx).await {
            Ok(result) => result.into_response(),
            Err(err) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error processing trace stats: {err}"),
            ),
        }
    }

    async fn dsm_proxy(
        State((config, _, _, _, _, _, tags_provider, client, api_key_factory)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            config,
            client,
            api_key_factory,
            tags_provider,
            request,
            "trace.agent",
            DSM_ENDPOINT_PATH,
            "DSM",
        )
        .await
    }

    async fn profiling_proxy(
        State((config, _, _, _, _, _, tags_provider, client, api_key_factory)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            config,
            client,
            api_key_factory,
            tags_provider,
            request,
            "intake.profile",
            PROFILING_BACKEND_PATH,
            "profiling",
        )
        .await
    }

    async fn llm_obs_eval_metric_proxy(
        State((config, _, _, _, _, _, tags_provider, client, api_key_factory)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            config,
            client,
            api_key_factory,
            tags_provider,
            request,
            "api",
            LLM_OBS_EVAL_METRIC_INTAKE_PATH,
            "llm_obs_eval_metric",
        )
        .await
    }

    async fn llm_obs_eval_metric_proxy_v2(
        State((config, _, _, _, _, _, tags_provider, client, api_key_factory)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            config,
            client,
            api_key_factory,
            tags_provider,
            request,
            "api",
            LLM_OBS_EVAL_METRIC_INTAKE_PATH_V2,
            "llm_obs_eval_metric",
        )
        .await
    }

    async fn llm_obs_spans_proxy(
        State((config, _, _, _, _, _, tags_provider, client, api_key_factory)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            config,
            client,
            api_key_factory,
            tags_provider,
            request,
            "llmobs-intake",
            LLM_OBS_SPANS_INTAKE_PATH,
            "llm_obs_spans",
        )
        .await
    }

    async fn debugger_logs_proxy(
        State((config, _, _, _, _, _, tags_provider, client, api_key_factory)): State<AgentState>,
        request: Request,
    ) -> Response {
        Self::handle_proxy(
            config,
            client,
            api_key_factory,
            tags_provider,
            request,
            "http-intake.logs",
            DEBUGGER_LOGS_INTAKE_PATH,
            "debugger_logs",
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
                    DEBUGGER_ENDPOINT_PATH,
                ],
                "client_drop_p0s": true,
            }
        );
        (StatusCode::OK, response_json.to_string()).into_response()
    }

    async fn handle_traces(
        config: Arc<config::Config>,
        request: Request,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendData>,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
        tags_provider: Arc<provider::Provider>,
        version: ApiVersion,
    ) -> Response {
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
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

        let mut reparenting_info = {
            let invocation_processor = invocation_processor.lock().await;
            invocation_processor.get_reparenting_info()
        };

        for chunk in &mut traces {
            for span in chunk.iter_mut() {
                // If the aws.lambda.load span is found, we're in Python or Node.
                // We need to update the trace ID of the cold start span, reparent the `aws.lambda.load`
                // span to the cold start span, and eventually send the cold start span.
                if span.name == LAMBDA_LOAD_SPAN {
                    let mut invocation_processor = invocation_processor.lock().await;
                    if let Some(cold_start_span_id) =
                        invocation_processor.set_cold_start_span_trace_id(span.trace_id)
                    {
                        span.parent_id = cold_start_span_id;
                    }
                }

                if span.resource == INVOCATION_SPAN_RESOURCE {
                    let mut invocation_processor = invocation_processor.lock().await;
                    invocation_processor.add_tracer_span(span);
                }
                handle_reparenting(&mut reparenting_info, span);
            }
        }

        {
            let mut invocation_processor = invocation_processor.lock().await;
            for ctx_to_send in invocation_processor.update_reparenting(reparenting_info) {
                debug!("Invocation span is now ready. Sending: {ctx_to_send:?}");
                invocation_processor
                    .send_ctx_spans(&tags_provider, &trace_processor, &trace_tx, ctx_to_send)
                    .await;
            }
        }

        let send_data = trace_processor
            .process_traces(
                config,
                tags_provider,
                tracer_header_tags,
                traces,
                body_size,
                None,
            )
            .await;

        // send trace payload to our trace flusher
        match trace_tx.send(send_data).await {
            Ok(()) => success_response("Successfully buffered traces to be flushed."),
            Err(err) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error sending traces to the trace flusher: {err}"),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_proxy(
        config: Arc<config::Config>,
        client: reqwest::Client,
        api_key_factory: Arc<ApiKeyFactory>,
        tags_provider: Arc<provider::Provider>,
        request: Request,
        backend_domain: &str,
        backend_path: &str,
        error_context: &str,
    ) -> Response {
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
        };

        let target_url = format!("https://{}.{}{}", backend_domain, config.site, backend_path);
        let mut request_builder = client.post(&target_url);

        for (name, value) in &parts.headers {
            if name.as_str().to_lowercase() != "host"
                && name.as_str().to_lowercase() != "content-length"
            {
                if let Ok(header_value) =
                    reqwest::header::HeaderValue::from_str(value.to_str().unwrap_or_default())
                {
                    request_builder = request_builder.header(name.as_str(), header_value);
                }
            }
        }
        let api_key = api_key_factory.get_api_key().await;
        request_builder = request_builder.header("DD-API-KEY", api_key);
        request_builder = request_builder.header(
            DD_ADDITIONAL_TAGS_HEADER,
            format!(
                "_dd.origin:lambda;functionname:{}",
                tags_provider
                    .get_canonical_resource_name()
                    .unwrap_or_default()
            ),
        );

        let response = match request_builder.body(body).send().await {
            Ok(resp) => resp,
            Err(err) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    format!("Error sending request to {error_context} backend: {err}"),
                );
            }
        };

        let status = StatusCode::from_u16(response.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        let mut builder = axum::response::Response::builder().status(status);

        for (name, value) in response.headers() {
            if let Ok(header_value) =
                axum::http::HeaderValue::from_str(value.to_str().unwrap_or_default())
            {
                builder = builder.header(name.as_str(), header_value);
            }
        }

        let response_body = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    format!("Error reading response from {error_context} backend: {err}"),
                );
            }
        };

        match builder.body(axum::body::Body::from(response_body)) {
            Ok(response) => response.into_response(),
            Err(err) => error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Error building response: {err}"),
            ),
        }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<SendData> {
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
