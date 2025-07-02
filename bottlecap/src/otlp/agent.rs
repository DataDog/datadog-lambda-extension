use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use datadog_trace_utils::send_data::SendData;
use datadog_trace_utils::trace_utils::TracerHeaderTags as DatadogTracerHeaderTags;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::{
    config::Config,
    http::{extract_request_body, handler_not_found},
    otlp::processor::Processor as OtlpProcessor,
    tags::provider,
    traces::trace_processor::TraceProcessor,
};

const OTLP_AGENT_HTTP_PORT: u16 = 4318;

type AgentState = (
    Arc<Config>,
    Arc<provider::Provider>,
    OtlpProcessor,
    Arc<dyn TraceProcessor + Send + Sync>,
    Sender<SendData>,
);

pub struct Agent {
    config: Arc<Config>,
    tags_provider: Arc<provider::Provider>,
    processor: OtlpProcessor,
    trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendData>,
    port: u16,
    shutdown_token: CancellationToken,
}

impl Agent {
    pub fn new(
        config: Arc<Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendData>,
    ) -> Self {
        let port = Self::parse_port(
            &config.otlp_config_receiver_protocols_http_endpoint,
            OTLP_AGENT_HTTP_PORT,
        );
        let shutdown_token = CancellationToken::new();

        Self {
            config: Arc::clone(&config),
            tags_provider: Arc::clone(&tags_provider),
            processor: OtlpProcessor::new(Arc::clone(&config)),
            trace_processor,
            trace_tx,
            port,
            shutdown_token,
        }
    }

    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    fn parse_port(endpoint: &Option<String>, default_port: u16) -> u16 {
        if let Some(endpoint) = endpoint {
            let port = endpoint.split(':').nth(1);
            if let Some(port) = port {
                return port.parse::<u16>().unwrap_or_else(|_| {
                    error!("Invalid OTLP port, using default port {default_port}");
                    default_port
                });
            }

            error!("Invalid OTLP endpoint format, using default port {default_port}");
        }

        default_port
    }

    pub fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let socket = SocketAddr::from(([127, 0, 0, 1], self.port));
        let router = self.make_router();

        let shutdown_token_clone = self.shutdown_token.clone();
        tokio::spawn(async move {
            let listener = TcpListener::bind(&socket)
                .await
                .expect("Failed to bind socket");
            debug!("OTLP | Starting collector on {}", socket);
            axum::serve(listener, router)
                .with_graceful_shutdown(Self::graceful_shutdown(shutdown_token_clone))
                .await
                .expect("Failed to start OTLP agent");
        });

        Ok(())
    }

    fn make_router(&self) -> Router {
        let state: AgentState = (
            Arc::clone(&self.config),
            Arc::clone(&self.tags_provider),
            self.processor.clone(),
            Arc::clone(&self.trace_processor),
            self.trace_tx.clone(),
        );

        Router::new()
            .route("/v1/traces", post(Self::v1_traces))
            .fallback(handler_not_found)
            .with_state(state)
    }

    async fn graceful_shutdown(shutdown_token: CancellationToken) {
        shutdown_token.cancelled().await;
        debug!("OTLP | Shutdown signal received, shutting down");
    }

    async fn v1_traces(
        State((config, tags_provider, processor, trace_processor, trace_tx)): State<AgentState>,
        request: Request,
    ) -> Response {
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("OTLP | Failed to extract request body: {e}"),
                )
                    .into_response();
            }
        };

        let traces = match processor.process(&body) {
            Ok(traces) => traces,
            Err(e) => {
                error!("OTLP | Failed to process request: {:?}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to process request: {e}"),
                )
                    .into_response();
            }
        };

        let tracer_header_tags: DatadogTracerHeaderTags = (&parts.headers).into();
        let body_size = size_of_val(&traces);
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

        if send_data.is_empty() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "message": "Not sending traces, processor returned empty data" })
                    .to_string(),
            )
                .into_response();
        }

        match trace_tx.send(send_data).await {
            Ok(()) => {
                debug!("OTLP | Successfully buffered traces to be flushed.");
                (
                    StatusCode::OK,
                    json!({"rate_by_service":{"service:,env:":1}}).to_string(),
                )
                    .into_response()
            }
            Err(err) => {
                error!("OTLP | Error sending traces to the trace flusher: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "message": format!("Error sending traces to the trace flusher: {err}") }).to_string()
                ).into_response()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port_with_valid_endpoint() {
        // Test with a valid endpoint containing a port
        let endpoint = Some("localhost:8080".to_string());
        assert_eq!(Agent::parse_port(&endpoint, OTLP_AGENT_HTTP_PORT), 8080);
    }

    #[test]
    fn test_parse_port_with_invalid_port_format() {
        // Test with an endpoint containing an invalid port format
        let endpoint = Some("localhost:invalid".to_string());
        assert_eq!(
            Agent::parse_port(&endpoint, OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_missing_port() {
        // Test with an endpoint missing a port
        let endpoint = Some("localhost".to_string());
        assert_eq!(
            Agent::parse_port(&endpoint, OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_none_endpoint() {
        // Test with None endpoint
        let endpoint: Option<String> = None;
        assert_eq!(
            Agent::parse_port(&endpoint, OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_empty_endpoint() {
        // Test with an empty endpoint
        let endpoint = Some("".to_string());
        assert_eq!(
            Agent::parse_port(&endpoint, OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }
}
