// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::{net::TcpListener, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::{
    http::extract_request_body,
    lifecycle::invocation::processor::Processor as InvocationProcessor,
    traces::propagation::text_map_propagator::{
        DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY, DATADOG_SAMPLING_PRIORITY_KEY, DATADOG_TAGS_KEY,
        DATADOG_TRACE_ID_KEY,
    },
};

const HELLO_PATH: &str = "/lambda/hello";
const START_INVOCATION_PATH: &str = "/lambda/start-invocation";
const END_INVOCATION_PATH: &str = "/lambda/end-invocation";
const AGENT_PORT: usize = 8124;

pub struct Listener {
    pub invocation_processor: Arc<Mutex<InvocationProcessor>>,
    pub shutdown_token: CancellationToken,
}

type ListenerState = (Arc<Mutex<InvocationProcessor>>, Arc<Mutex<JoinSet<()>>>);

impl Listener {
    pub fn new(invocation_processor: Arc<Mutex<InvocationProcessor>>) -> Self {
        let shutdown_token = CancellationToken::new();
        Self {
            invocation_processor,
            shutdown_token,
        }
    }

    #[must_use]
    pub fn get_shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let port = u16::try_from(AGENT_PORT).expect("AGENT_PORT is too large");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = TcpListener::bind(&addr).await?;

        let tasks = Arc::new(Mutex::new(JoinSet::new()));
        let state: ListenerState = (self.invocation_processor.clone(), tasks.clone());
        let router = Self::make_router(state);

        debug!("Lifecycle API | Starting listener on {}", addr);
        axum::serve(listener, router)
            .with_graceful_shutdown(Self::graceful_shutdown(tasks, self.shutdown_token.clone()))
            .await?;
        Ok(())
    }

    fn make_router(state: ListenerState) -> Router {
        Router::new()
            .route(START_INVOCATION_PATH, post(Self::handle_start_invocation))
            .route(END_INVOCATION_PATH, post(Self::handle_end_invocation))
            .route(HELLO_PATH, get(Self::handle_hello))
            .with_state(state)
    }

    async fn graceful_shutdown(tasks: Arc<Mutex<JoinSet<()>>>, shutdown_token: CancellationToken) {
        shutdown_token.cancelled().await;
        debug!("Lifecycle API | Shutdown signal received, shutting down");

        let mut tasks = tasks.lock().await;
        while let Some(task) = tasks.join_next().await {
            if let Some(e) = task.err() {
                error!("Lifecycle API | Shutdown error: {e}");
            }
        }
    }

    // TODO(duncanista): spawn task to handle start invocation request
    async fn handle_start_invocation(
        State((invocation_processor, _tasks)): State<ListenerState>,
        request: Request,
    ) -> Response {
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to extract request body: {e}");
                return (
                    StatusCode::BAD_REQUEST,
                    "Could not read start invocation request body",
                )
                    .into_response();
            }
        };

        let (_, response) =
            Self::universal_instrumentation_start(&parts.headers, body, invocation_processor).await;

        response
    }

    async fn handle_end_invocation(
        State((invocation_processor, tasks)): State<ListenerState>,
        request: Request,
    ) -> Response {
        let mut join_set = tasks.lock().await;
        join_set.spawn(async move {
            let (parts, body) = match extract_request_body(request).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Failed to extract request body: {e}");
                    return;
                }
            };

            if let Err(e) =
                Self::universal_instrumentation_end(&parts.headers, body, invocation_processor)
                    .await
            {
                error!("Failed to process end invocation request {e}");
            }
        });

        (StatusCode::OK, json!({}).to_string()).into_response()
    }

    #[allow(clippy::unused_async)]
    async fn handle_hello() -> Response {
        warn!("[DEPRECATED] Please upgrade your tracing library, the /hello route is deprecated");
        (StatusCode::OK, json!({}).to_string()).into_response()
    }

    pub async fn universal_instrumentation_start(
        headers: &HeaderMap,
        body: Bytes,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> (u64, Response) {
        debug!("Received start invocation request");
        let body = body.to_vec();

        let headers = Self::headers_to_map(headers);

        let extracted_span_context = {
            let mut processor = invocation_processor.lock().await;
            processor.on_universal_instrumentation_start(headers, body)
        };

        let found_parent_span_id;

        // If a `SpanContext` exists, then tell the tracer to use it.
        // todo: update this whole code with DatadogHeaderPropagator::inject
        // since this logic looks messy
        let response = if let Some(sp) = extracted_span_context {
            let mut headers = HeaderMap::new();
            headers.insert(
                DATADOG_TRACE_ID_KEY,
                sp.trace_id
                    .to_string()
                    .parse()
                    .expect("Failed to parse trace id"),
            );
            if let Some(priority) = sp.sampling.and_then(|s| s.priority) {
                headers.insert(
                    DATADOG_SAMPLING_PRIORITY_KEY,
                    priority
                        .to_string()
                        .parse()
                        .expect("Failed to parse sampling priority"),
                );
            }

            // Handle 128 bit trace ids
            if let Some(trace_id_higher_order_bits) =
                sp.tags.get(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY)
            {
                headers.insert(
                    DATADOG_TAGS_KEY,
                    format!(
                        "{DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY}={trace_id_higher_order_bits}"
                    )
                    .parse()
                    .expect("Failed to parse tags"),
                );
            }
            found_parent_span_id = sp.span_id;

            (StatusCode::OK, headers, json!({}).to_string()).into_response()
        } else {
            found_parent_span_id = 0;
            (StatusCode::OK, json!({}).to_string()).into_response()
        };

        (found_parent_span_id, response)
    }

    pub async fn universal_instrumentation_end(
        headers: &HeaderMap,
        body: Bytes,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        debug!("Received end invocation request");
        let body = body.to_vec();
        let mut processor = invocation_processor.lock().await;

        let headers = Self::headers_to_map(headers);
        processor.on_universal_instrumentation_end(headers, body);

        Ok(())
    }

    fn headers_to_map(headers: &HeaderMap) -> HashMap<String, String> {
        headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }
}
