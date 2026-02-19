use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use libdd_trace_utils::trace_utils::TracerHeaderTags as DatadogTracerHeaderTags;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
use prost::Message;
use std::mem::size_of_val;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tonic_types::Status;
use tracing::{debug, error};

use crate::{
    config::Config,
    http::{extract_request_body, handler_not_found},
    otlp::processor::Processor as OtlpProcessor,
    tags::provider,
    traces::{
        stats_generator::StatsGenerator, trace_aggregator::SendDataBuilderInfo,
        trace_processor::TraceProcessor,
    },
};

const OTLP_AGENT_HTTP_PORT: u16 = 4318;

type AgentState = (
    Arc<Config>,
    Arc<provider::Provider>,
    OtlpProcessor,
    Arc<dyn TraceProcessor + Send + Sync>,
    Sender<SendDataBuilderInfo>,
    Arc<StatsGenerator>,
);

pub struct Agent {
    config: Arc<Config>,
    tags_provider: Arc<provider::Provider>,
    processor: OtlpProcessor,
    trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendDataBuilderInfo>,
    stats_generator: Arc<StatsGenerator>,
    port: u16,
    cancel_token: CancellationToken,
}

impl Agent {
    pub fn new(
        config: Arc<Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendDataBuilderInfo>,
        stats_generator: Arc<StatsGenerator>,
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
            stats_generator,
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

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let socket = SocketAddr::from(([127, 0, 0, 1], self.port));
        let router = self.make_router();

        let cancel_token_clone = self.cancel_token.clone();

        let listener = TcpListener::bind(&socket)
            .await
            .expect("Failed to bind socket");
        debug!("OTLP | Starting collector on {}", socket);
        axum::serve(listener, router)
            .with_graceful_shutdown(Self::graceful_shutdown(cancel_token_clone))
            .await
            .expect("Failed to start OTLP agent");

        Ok(())
    }

    fn make_router(&self) -> Router {
        let state: AgentState = (
            Arc::clone(&self.config),
            Arc::clone(&self.tags_provider),
            self.processor.clone(),
            Arc::clone(&self.trace_processor),
            self.trace_tx.clone(),
            Arc::clone(&self.stats_generator),
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
        State((config, tags_provider, processor, trace_processor, trace_tx, stats_generator)): State<
            AgentState,
        >,
        request: Request,
    ) -> Response {
        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                error!("OTLP | Failed to extract request body: {e}");
                return Self::otlp_error_response(
                    StatusCode::BAD_REQUEST,
                    format!("Failed to extract request body: {e}"),
                );
            }
        };

        let traces = match processor.process(&body) {
            Ok(traces) => traces,
            Err(e) => {
                error!("OTLP | Failed to process request: {:?}", e);
                return Self::otlp_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to process request: {e}"),
                );
            }
        };

        let tracer_header_tags: DatadogTracerHeaderTags = (&parts.headers).into();
        let body_size = size_of_val(&traces);
        if body_size == 0 {
            error!("OTLP | Not sending traces, processor returned empty data");
            return Self::otlp_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Not sending traces, processor returned empty data".to_string(),
            );
        }

        let compute_trace_stats_on_extension = config.compute_trace_stats_on_extension;
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
                return Self::otlp_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Error sending traces to the trace aggregator: {err}"),
                );
            }
        }

        // This needs to be after process_traces() because process_traces()
        // performs obfuscation, and we need to compute stats on the obfuscated traces.
        if compute_trace_stats_on_extension
            && let Err(err) = stats_generator.send(&processed_traces)
        {
            // Just log the error. We don't think trace stats are critical, so we don't want to
            // return an error if only stats fail to send.
            error!("OTLP | Error sending traces to the stats concentrator: {err}");
        }

        Self::otlp_success_response()
    }

    fn otlp_error_response(status_code: StatusCode, message: String) -> Response {
        let status = Status {
            code: i32::from(status_code.as_u16()),
            message: message.clone(),
            details: vec![],
        };

        let mut buf = Vec::new();
        if let Err(e) = status.encode(&mut buf) {
            error!("OTLP | Failed to encode error response: {e}");
            return (status_code, [(header::CONTENT_TYPE, "text/plain")], message).into_response();
        }

        (
            status_code,
            [(header::CONTENT_TYPE, "application/x-protobuf")],
            buf,
        )
            .into_response()
    }

    fn otlp_success_response() -> Response {
        let response = ExportTraceServiceResponse {
            partial_success: None,
        };

        let mut buf = Vec::new();
        if let Err(e) = response.encode(&mut buf) {
            error!("OTLP | Failed to encode success response: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to encode response".to_string(),
            )
                .into_response();
        }

        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/x-protobuf")],
            buf,
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
