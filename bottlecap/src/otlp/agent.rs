use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
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
    traces::{
        trace_aggregator::SendDataBuilderInfo, trace_processor::TraceProcessor,
        trace_stats_processor::SendingTraceStatsProcessor,
    },
};

const OTLP_AGENT_HTTP_PORT: u16 = 4318;

type AgentState = (
    Arc<Config>,
    Arc<provider::Provider>,
    OtlpProcessor,
    Arc<dyn TraceProcessor + Send + Sync>,
    Sender<SendDataBuilderInfo>,
    Arc<SendingTraceStatsProcessor>,
);

pub struct Agent {
    config: Arc<Config>,
    tags_provider: Arc<provider::Provider>,
    processor: OtlpProcessor,
    trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendDataBuilderInfo>,
    stats_sender: Arc<SendingTraceStatsProcessor>,
    port: u16,
    cancel_token: CancellationToken,
}

impl Agent {
    pub fn new(
        config: Arc<Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendDataBuilderInfo>,
        stats_sender: Arc<SendingTraceStatsProcessor>,
    ) -> Self {
        let port = Self::parse_port(
            config.otlp_config_receiver_protocols_http_endpoint.as_ref(),
            OTLP_AGENT_HTTP_PORT,
        );
        let cancel_token = CancellationToken::new();

        Self {
            config: Arc::clone(&config),
            tags_provider: Arc::clone(&tags_provider),
            processor: OtlpProcessor::new(Arc::clone(&config)),
            trace_processor,
            trace_tx,
            stats_sender,
            port,
            cancel_token,
        }
    }

    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    fn parse_port(endpoint: Option<&String>, default_port: u16) -> u16 {
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

        let cancel_token_clone = self.cancel_token.clone();
        tokio::spawn(async move {
            let listener = TcpListener::bind(&socket)
                .await
                .expect("Failed to bind socket");
            debug!("OTLP | Starting collector on {}", socket);
            axum::serve(listener, router)
                .with_graceful_shutdown(Self::graceful_shutdown(cancel_token_clone))
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
            Arc::clone(&self.stats_sender),
        );

        Router::new()
            .route("/v1/traces", post(Self::v1_traces))
            .fallback(handler_not_found)
            .with_state(state)
    }

    async fn graceful_shutdown(cancel_token: CancellationToken) {
        cancel_token.cancelled().await;
        debug!("OTLP | Shutdown signal received, shutting down");
    }

    async fn v1_traces(
        State((config, tags_provider, processor, trace_processor, trace_tx, stats_sender)): State<
            AgentState,
        >,
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
        if body_size == 0 {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "message": "Not sending traces, processor returned empty data" })
                    .to_string(),
            )
                .into_response();
        }

        let compute_trace_stats = config.compute_trace_stats;
        let (send_data_builder, processed_traces) = trace_processor.process_traces(
            config,
            tags_provider,
            tracer_header_tags,
            traces,
            body_size,
            None,
        );

        match trace_tx.send(send_data_builder).await {
            Ok(()) => {
                debug!("OTLP | Successfully buffered traces to be aggregated.");
            }
            Err(err) => {
                error!("OTLP | Error sending traces to the trace aggregator: {err}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "message": format!("Error sending traces to the trace aggregator: {err}") }).to_string()
                ).into_response();
            }
        };

        // This needs to be after process_traces() because process_traces()
        // performs obfuscation, and we need to compute stats on the obfuscated traces.
        if compute_trace_stats {
            if let Err(err) = stats_sender.send(&processed_traces) {
                error!("OTLP | Error sending traces to the stats concentrator: {err}");
                return (
                StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "message": format!("Error sending traces to the stats concentrator: {err}") }).to_string()
                ).into_response();
            }
        }

        (
            StatusCode::OK,
            json!({"rate_by_service":{"service:,env:":1}}).to_string(),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port_with_valid_endpoint() {
        // Test with a valid endpoint containing a port
        let endpoint = Some("localhost:8080".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            8080
        );
    }

    #[test]
    fn test_parse_port_with_invalid_port_format() {
        // Test with an endpoint containing an invalid port format
        let endpoint = Some("localhost:invalid".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_missing_port() {
        // Test with an endpoint missing a port
        let endpoint = Some("localhost".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_none_endpoint() {
        // Test with None endpoint
        let endpoint: Option<String> = None;
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_empty_endpoint() {
        // Test with an empty endpoint
        let endpoint = Some(String::new());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }
}
