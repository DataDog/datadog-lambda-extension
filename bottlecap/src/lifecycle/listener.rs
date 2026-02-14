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
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::{net::TcpListener, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::{
    http::{extract_request_body, headers_to_map},
    lifecycle::invocation::processor_service::InvocationProcessorHandle,
    traces::{
        context::SpanContext,
        propagation::{
            DatadogCompositePropagator,
            text_map_propagator::{
                DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY, DATADOG_SAMPLING_PRIORITY_KEY,
                DATADOG_TAGS_KEY, DATADOG_TRACE_ID_KEY,
            },
        },
    },
};

const HELLO_PATH: &str = "/lambda/hello";
const START_INVOCATION_PATH: &str = "/lambda/start-invocation";
const END_INVOCATION_PATH: &str = "/lambda/end-invocation";
const AGENT_PORT: usize = 8124;

/// Extracts the AWS Lambda request ID from the LWA proxy header.
fn extract_request_id_from_headers(headers: &HashMap<String, String>) -> Option<String> {
    headers
        .get("lambda-runtime-aws-request-id")
        .map(String::to_string)
}

pub struct Listener {
    propagator: Arc<DatadogCompositePropagator>,
    pub invocation_processor_handle: InvocationProcessorHandle,
    pub shutdown_token: CancellationToken,
}

type ListenerState = (
    InvocationProcessorHandle,
    Arc<DatadogCompositePropagator>,
    Arc<Mutex<JoinSet<()>>>,
);

impl Listener {
    #[must_use]
    pub fn new(
        invocation_processor_handle: InvocationProcessorHandle,
        propagator: Arc<DatadogCompositePropagator>,
    ) -> Self {
        let shutdown_token = CancellationToken::new();
        Self {
            propagator,
            invocation_processor_handle,
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
        let state: ListenerState = (
            self.invocation_processor_handle.clone(),
            self.propagator.clone(),
            tasks.clone(),
        );
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
            if let Err(e) = task {
                error!("Lifecycle API | Shutdown error: {e}");
            }
        }
    }

    // TODO(duncanista): spawn task to handle start invocation request
    async fn handle_start_invocation(
        State((invocation_processor_handle, propagator, tasks)): State<ListenerState>,
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

        let headers = headers_to_map(&parts.headers);
        let payload_value = serde_json::from_slice::<Value>(&body).unwrap_or_else(|_| json!({}));

        let mut response_headers = HeaderMap::new();
        let extracted_span_context = InvocationProcessorHandle::extract_span_context(
            &headers,
            &payload_value,
            propagator.clone(),
        );

        if let Some(sp) = &extracted_span_context {
            Self::inject_span_context_to_headers(&mut response_headers, sp);
        }

        let mut join_set = tasks.lock().await;
        join_set.spawn(async move {
            Self::universal_instrumentation_start(
                headers,
                payload_value,
                invocation_processor_handle,
            )
            .await;
        });

        (StatusCode::OK, response_headers, json!({}).to_string()).into_response()
    }

    async fn handle_end_invocation(
        State((invocation_processor_handle, _, tasks)): State<ListenerState>,
        request: Request,
    ) -> Response {
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to extract request body: {e}");
                return (StatusCode::OK, json!({}).to_string()).into_response();
            }
        };

        let mut join_set = tasks.lock().await;
        join_set.spawn(async move {
            Self::universal_instrumentation_end(&parts.headers, body, invocation_processor_handle)
                .await;
        });

        (StatusCode::OK, json!({}).to_string()).into_response()
    }

    #[allow(clippy::unused_async)]
    async fn handle_hello() -> Response {
        warn!("[DEPRECATED] Please upgrade your tracing library, the /hello route is deprecated");
        (StatusCode::OK, json!({}).to_string()).into_response()
    }

    pub async fn universal_instrumentation_start(
        headers: HashMap<String, String>,
        payload_value: Value,
        invocation_processor_handle: InvocationProcessorHandle,
    ) {
        debug!(
            "Received start invocation request from headers:{headers:?}, payload_value:{payload_value:?}"
        );

        let request_id = extract_request_id_from_headers(&headers);

        if let Err(e) = invocation_processor_handle
            .on_universal_instrumentation_start(headers, payload_value, request_id)
            .await
        {
            error!(
                "Failed to send universal instrumentation start to processor: {}",
                e
            );
        }
    }

    // If a `SpanContext` exists, then tell the tracer to use it.
    // todo: update this whole code with DatadogHeaderPropagator::inject
    // since this logic looks messy
    fn inject_span_context_to_headers(headers: &mut HeaderMap, span_context: &SpanContext) {
        headers.insert(
            DATADOG_TRACE_ID_KEY,
            span_context
                .trace_id
                .to_string()
                .parse()
                .expect("Failed to parse trace id"),
        );

        if let Some(priority) = span_context.sampling.and_then(|s| s.priority) {
            headers.insert(
                DATADOG_SAMPLING_PRIORITY_KEY,
                priority
                    .to_string()
                    .parse()
                    .expect("Failed to parse sampling priority"),
            );
        }

        // Handle 128 bit trace ids
        if let Some(trace_id_higher_order_bits) = span_context
            .tags
            .get(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY)
        {
            headers.insert(
                DATADOG_TAGS_KEY,
                format!("{DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY}={trace_id_higher_order_bits}")
                    .parse()
                    .expect("Failed to parse tags"),
            );
        }
    }

    pub async fn universal_instrumentation_end(
        headers: &HeaderMap,
        body: Bytes,
        invocation_processor_handle: InvocationProcessorHandle,
    ) {
        let headers = headers_to_map(headers);
        let payload_value = serde_json::from_slice::<Value>(&body).unwrap_or_else(|_| json!({}));

        debug!(
            "Received end invocation request from headers:{headers:?}, payload_value:{payload_value:?}"
        );
        let request_id = extract_request_id_from_headers(&headers);

        if let Err(e) = invocation_processor_handle
            .on_universal_instrumentation_end(headers, payload_value, request_id)
            .await
        {
            error!(
                "Failed to send universal instrumentation end to processor: {}",
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_request_id_from_header() {
        let headers = HashMap::from([(
            "lambda-runtime-aws-request-id".to_string(),
            "req-from-header".to_string(),
        )]);

        let result = extract_request_id_from_headers(&headers);

        assert_eq!(
            result,
            Some("req-from-header".to_string()),
            "Should extract request_id from lambda-runtime-aws-request-id header"
        );
    }

    #[test]
    fn test_extract_request_id_none_when_missing() {
        let headers = HashMap::new();

        let result = extract_request_id_from_headers(&headers);

        assert_eq!(
            result, None,
            "Should return None when request_id header is not present"
        );
    }

    #[test]
    fn test_extract_request_id_lwa_proxy_header() {
        let headers = HashMap::from([(
            "lambda-runtime-aws-request-id".to_string(),
            "lwa-proxy-request-id".to_string(),
        )]);

        let result = extract_request_id_from_headers(&headers);

        assert_eq!(
            result,
            Some("lwa-proxy-request-id".to_string()),
            "Should extract request_id from LWA proxy header"
        );
    }
}
