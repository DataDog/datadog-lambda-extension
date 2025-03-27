// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use hyper::service::{make_service_fn, service_fn};
use hyper::{http, Body, Method, Request, Response, Server, StatusCode};
use reqwest;
use serde_json::json;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex as SyncMutex;
use std::time::Instant;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::Mutex;
use tracing::{debug, error};

use crate::config;
use crate::http_client;
use crate::lifecycle::invocation::context::ContextBuffer;
use crate::tags::provider;
use crate::traces::{stats_aggregator, stats_processor, trace_aggregator, trace_processor};
use datadog_trace_mini_agent::http_utils::{
    self, log_and_create_http_response, log_and_create_traces_success_http_response,
};
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils::{self, SendData};

const TRACE_AGENT_PORT: usize = 8126;
const V4_TRACE_ENDPOINT_PATH: &str = "/v0.4/traces";
const V5_TRACE_ENDPOINT_PATH: &str = "/v0.5/traces";
const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";
const DSM_ENDPOINT_PATH: &str = "/api/v0.1/pipeline_stats";
const DSM_AGENT_PATH: &str = "/v0.1/pipeline_stats";
const PROFILING_ENDPOINT_PATH: &str = "/profiling/v1/input";
const PROFILING_BACKEND_PATH: &str = "/api/v2/profile";
const LLM_OBS_EVAL_METRIC_ENDPOINT_PATH: &str = "/evp_proxy/v2/api/intake/llm-obs/v1/eval-metric";
const LLM_OBS_SPANS_ENDPOINT_PATH: &str = "/evp_proxy/v2/api/v2/llmobs";
const DD_ADDITIONAL_TAGS_HEADER: &str = "X-Datadog-Additional-Tags";
const INFO_ENDPOINT_PATH: &str = "/info";
const TRACER_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;
const STATS_PAYLOAD_CHANNEL_BUFFER_SIZE: usize = 10;
pub const MAX_CONTENT_LENGTH: usize = 10 * 1024 * 1024;

pub struct TraceAgent {
    pub config: Arc<config::Config>,
    pub trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    pub stats_aggregator: Arc<Mutex<stats_aggregator::StatsAggregator>>,
    pub stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
    pub tags_provider: Arc<provider::Provider>,
    pub context_buffer: Arc<SyncMutex<ContextBuffer>>,
    http_client: reqwest::Client,
    api_key: String,
    tx: Sender<SendData>,
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
        tags_provider: Arc<provider::Provider>,
        context_buffer: Arc<SyncMutex<ContextBuffer>>,
        resolved_api_key: String,
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
            tags_provider,
            context_buffer,
            http_client: http_client::get_client(config),
            tx: trace_tx,
            api_key: resolved_api_key,
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let now = Instant::now();
        let trace_tx = self.tx.clone();

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

        // setup our hyper http server, where the endpoint_handler handles incoming requests
        let trace_processor = self.trace_processor.clone();
        let stats_processor = self.stats_processor.clone();
        let endpoint_config = self.config.clone();
        let tags_provider = self.tags_provider.clone();
        let context_buffer = self.context_buffer.clone();
        let client = self.http_client.clone();
        let api_key = self.api_key.clone();

        let make_svc = make_service_fn(move |_| {
            let trace_processor = trace_processor.clone();
            let trace_tx = trace_tx.clone();

            let stats_processor = stats_processor.clone();
            let stats_tx = stats_tx.clone();

            let endpoint_config = endpoint_config.clone();
            let tags_provider = tags_provider.clone();
            let context_buffer = context_buffer.clone();
            let client = client.clone();
            let api_key = api_key.clone();

            let service = service_fn(move |req: Request<Body>| {
                TraceAgent::trace_endpoint_handler(
                    endpoint_config.clone(),
                    req,
                    trace_processor.clone(),
                    trace_tx.clone(),
                    stats_processor.clone(),
                    stats_tx.clone(),
                    tags_provider.clone(),
                    context_buffer.clone(),
                    client.clone(),
                    api_key.clone(),
                )
            });

            async move { Ok::<_, Infallible>(service) }
        });

        let port = u16::try_from(TRACE_AGENT_PORT).expect("TRACE_AGENT_PORT is too large");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let server_builder = Server::try_bind(&addr)?;

        let server = server_builder.serve(make_svc);

        debug!("Trace Agent started: listening on port {TRACE_AGENT_PORT}");
        debug!(
            "Time taken start the Trace Agent: {} ms",
            now.elapsed().as_millis()
        );

        // start hyper http server
        if let Err(e) = server.await {
            error!("Server error: {e}");
            return Err(e.into());
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn trace_endpoint_handler(
        config: Arc<config::Config>,
        req: Request<Body>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendData>,
        stats_processor: Arc<dyn stats_processor::StatsProcessor + Send + Sync>,
        stats_tx: Sender<pb::ClientStatsPayload>,
        tags_provider: Arc<provider::Provider>,
        context_buffer: Arc<SyncMutex<ContextBuffer>>,
        client: reqwest::Client,
        api_key: String,
    ) -> http::Result<Response<Body>> {
        match (req.method(), req.uri().path()) {
            (&Method::PUT | &Method::POST, V4_TRACE_ENDPOINT_PATH) => match Self::handle_traces(
                config,
                req,
                trace_processor.clone(),
                trace_tx,
                tags_provider,
                ApiVersion::V04,
                context_buffer.clone(),
            )
            .await
            {
                Ok(result) => Ok(result),
                Err(err) => log_and_create_http_response(
                    &format!("Error processing traces: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            },
            (&Method::PUT | &Method::POST, V5_TRACE_ENDPOINT_PATH) => match Self::handle_traces(
                config,
                req,
                trace_processor.clone(),
                trace_tx,
                tags_provider,
                ApiVersion::V05,
                context_buffer.clone(),
            )
            .await
            {
                Ok(result) => Ok(result),
                Err(err) => log_and_create_http_response(
                    &format!("Error processing traces: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            },
            (&Method::PUT | &Method::POST, STATS_ENDPOINT_PATH) => {
                match stats_processor.process_stats(req, stats_tx).await {
                    Ok(result) => Ok(result),
                    Err(err) => log_and_create_http_response(
                        &format!("Error processing trace stats: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                }
            }
            (&Method::POST, DSM_AGENT_PATH) => {
                match Self::handle_dsm_proxy(config, tags_provider, api_key, client, req).await {
                    Ok(result) => Ok(result),
                    Err(err) => log_and_create_http_response(
                        &format!("DSM endpoint error: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                }
            }
            (&Method::POST, PROFILING_ENDPOINT_PATH) => {
                match Self::handle_profiling_proxy(config, tags_provider, api_key, client, req)
                    .await
                {
                    Ok(result) => Ok(result),
                    Err(err) => log_and_create_http_response(
                        &format!("Profiling endpoint error: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                }
            }
            (&Method::POST, LLM_OBS_EVAL_METRIC_ENDPOINT_PATH) => {
                match Self::handle_llm_obs_eval_metric_proxy(
                    config,
                    tags_provider,
                    api_key,
                    client,
                    req,
                )
                .await
                {
                    Ok(result) => Ok(result),
                    Err(err) => log_and_create_http_response(
                        &format!("LLM OBS Eval Metric endpoint error: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                }
            }
            (&Method::POST, LLM_OBS_SPANS_ENDPOINT_PATH) => {
                match Self::handle_llm_obs_spans_proxy(
                    config,
                    tags_provider,
                    api_key,
                    client,
                    req,
                )
                .await
                {
                    Ok(result) => Ok(result),
                    Err(err) => log_and_create_http_response(
                        &format!("LLM OBS Spans endpoint error: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    ),
                }
            }
            (_, INFO_ENDPOINT_PATH) => match Self::info_handler() {
                Ok(result) => Ok(result),
                Err(err) => log_and_create_http_response(
                    &format!("Info endpoint error: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            },
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }

    async fn handle_traces(
        config: Arc<config::Config>,
        req: Request<Body>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendData>,
        tags_provider: Arc<provider::Provider>,
        version: ApiVersion,
        context_buffer: Arc<SyncMutex<ContextBuffer>>,
    ) -> http::Result<Response<Body>> {
        let (parts, body) = req.into_parts();

        if let Some(response) = http_utils::verify_request_content_length(
            &parts.headers,
            MAX_CONTENT_LENGTH,
            "Error processing traces",
        ) {
            return response;
        }

        let tracer_header_tags = (&parts.headers).into();

        let (body_size, traces) = match version {
            ApiVersion::V04 => match trace_utils::get_traces_from_request_body(body).await {
                Ok(result) => result,
                Err(err) => {
                    return log_and_create_http_response(
                        &format!("Error deserializing trace from request body: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            },
            ApiVersion::V05 => match trace_utils::get_v05_traces_from_request_body(body).await {
                Ok(result) => result,
                Err(err) => {
                    return log_and_create_http_response(
                        &format!("Error deserializing trace from request body: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            },
        };

        let send_data = trace_processor.process_traces(
            config,
            tags_provider,
            tracer_header_tags,
            traces,
            body_size,
            None,
            context_buffer,
        );

        // send trace payload to our trace flusher
        match trace_tx.send(send_data).await {
            Ok(()) => log_and_create_traces_success_http_response(
                "Successfully buffered traces to be flushed.",
                StatusCode::OK,
            ),
            Err(err) => log_and_create_http_response(
                &format!("Error sending traces to the trace flusher: {err}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        }
    }

    fn info_handler() -> http::Result<Response<Body>> {
        let response_json = json!(
            {
                "endpoints": [
                    V4_TRACE_ENDPOINT_PATH,
                    STATS_ENDPOINT_PATH,
                    DSM_AGENT_PATH,
                    PROFILING_ENDPOINT_PATH,
                    INFO_ENDPOINT_PATH
                ],
                "client_drop_p0s": true,
            }
        );
        Response::builder()
            .status(200)
            .body(Body::from(response_json.to_string()))
    }

    /// Generic proxy handler for forwarding requests to Datadog backends
    #[allow(clippy::too_many_arguments)]
    async fn handle_proxy(
        config: Arc<config::Config>,
        client: reqwest::Client,
        api_key: String,
        tags_provider: Arc<provider::Provider>,
        req: Request<Body>,
        backend_domain: &str,
        backend_path: &str,
        error_context: &str,
    ) -> http::Result<Response<Body>> {
        let (parts, body) = req.into_parts();

        // TODO: Update this when upgrading hyper
        #[allow(deprecated)]
        let body_bytes = match hyper::body::to_bytes(body).await {
            Ok(bytes) => bytes,
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error reading request body: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
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
        let response = match request_builder.body(body_bytes).send().await {
            Ok(resp) => resp,
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error sending request to {error_context} backend: {err}"),
                    StatusCode::BAD_GATEWAY,
                );
            }
        };

        let status = StatusCode::from_u16(response.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        let mut builder = Response::builder().status(status);

        for (name, value) in response.headers() {
            if let Ok(header_value) =
                http::header::HeaderValue::from_str(value.to_str().unwrap_or_default())
            {
                builder = builder.header(name.as_str(), header_value);
            }
        }

        let response_body = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error reading response from {error_context} backend: {err}"),
                    StatusCode::BAD_GATEWAY,
                );
            }
        };

        builder.body(Body::from(response_body))
    }

    async fn handle_dsm_proxy(
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        api_key: String,
        client: reqwest::Client,
        req: Request<Body>,
    ) -> http::Result<Response<Body>> {
        Self::handle_proxy(
            config,
            client,
            api_key,
            tags_provider,
            req,
            "trace.agent",
            DSM_ENDPOINT_PATH,
            "DSM",
        )
        .await
    }

    async fn handle_profiling_proxy(
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        api_key: String,
        client: reqwest::Client,
        req: Request<Body>,
    ) -> http::Result<Response<Body>> {
        Self::handle_proxy(
            config,
            client,
            api_key,
            tags_provider,
            req,
            "intake.profile",
            PROFILING_BACKEND_PATH,
            "profiling",
        )
        .await
    }

    async fn handle_llm_obs_eval_metric_proxy(
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        api_key: String,
        client: reqwest::Client,
        req: Request<Body>,
    ) -> http::Result<Response<Body>> {
        Self::handle_proxy(
            config,
            client,
            api_key,
            tags_provider,
            req,
            "api",
            "/api/intake/llm-obs/v1/eval-metric",
            "llm_obs_eval_metric",
        )
        .await
    }

    async fn handle_llm_obs_spans_proxy(
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        api_key: String,
        client: reqwest::Client,
        req: Request<Body>,
    ) -> http::Result<Response<Body>> {
        Self::handle_proxy(
            config,
            client,
            api_key,
            tags_provider,
            req,
            "llmobs-intake",
            "/api/intake/llm-obs",
            "LLM OBS Spans",
        )
        .await
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<SendData> {
        self.tx.clone()
    }
}
