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
        if traces.is_empty() {
            error!("OTLP | Not sending traces, processor returned empty data");
            return Self::otlp_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Not sending traces, processor returned empty data".to_string(),
            );
        }
        let body_size = body.len();

        let compute_trace_stats_on_extension = config.compute_trace_stats_on_extension;
        let (send_data_builder, processed_traces) = trace_processor.process_traces(
            config,
            tags_provider,
            tracer_header_tags,
            traces,
            body_size,
            None,
        );

        if let Some(send_data_builder) = send_data_builder {
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
    use crate::config::Config;
    use crate::tags::provider::Provider;
    use crate::traces::{
        span_pointers::SpanPointer,
        stats_concentrator_service::StatsConcentratorService,
        stats_generator::StatsGenerator,
        trace_aggregator::{OwnedTracerHeaderTags, SendDataBuilderInfo},
        trace_processor::TraceProcessor,
    };
    use async_trait::async_trait;
    use axum::http::Request;
    use axum::body::Body;
    use libdd_trace_protobuf::pb;
    use libdd_trace_utils::tracer_header_tags::TracerHeaderTags;
    use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use prost::Message;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    struct NoopTraceProcessor {
        captured_body_size: std::sync::Mutex<Option<usize>>,
    }

    impl NoopTraceProcessor {
        fn new() -> Self {
            Self {
                captured_body_size: std::sync::Mutex::new(None),
            }
        }

        fn captured_body_size(&self) -> Option<usize> {
            *self.captured_body_size.lock().unwrap()
        }
    }

    #[async_trait]
    impl TraceProcessor for NoopTraceProcessor {
        fn process_traces(
            &self,
            _config: Arc<Config>,
            _tags_provider: Arc<Provider>,
            _header_tags: TracerHeaderTags<'_>,
            _traces: Vec<Vec<pb::Span>>,
            body_size: usize,
            _span_pointers: Option<Vec<SpanPointer>>,
        ) -> (Option<SendDataBuilderInfo>, TracerPayloadCollection) {
            *self.captured_body_size.lock().unwrap() = Some(body_size);
            (None, TracerPayloadCollection::V07(vec![]))
        }
    }

    fn make_state(trace_processor: Arc<dyn TraceProcessor + Send + Sync>) -> AgentState {
        let config = Arc::new(Config::default());
        let tags_provider = Arc::new(Provider::new(
            config.clone(),
            "lambda".to_string(),
            &std::collections::HashMap::new(),
        ));
        let otlp_processor = OtlpProcessor::new(config.clone());
        let (tx, _rx) = mpsc::channel(16);
        let (_, concentrator_handle) = StatsConcentratorService::new(config.clone());
        let stats_generator = Arc::new(StatsGenerator::new(concentrator_handle));
        (config, tags_provider, otlp_processor, trace_processor, tx, stats_generator)
    }

    fn make_router_with_processor(trace_processor: Arc<dyn TraceProcessor + Send + Sync>) -> axum::Router {
        let state = make_state(trace_processor);
        axum::Router::new()
            .route("/v1/traces", axum::routing::post(Agent::v1_traces))
            .with_state(state)
    }

    fn encode_otlp_request(request: &ExportTraceServiceRequest) -> Vec<u8> {
        let mut buf = Vec::new();
        request.encode(&mut buf).unwrap();
        buf
    }

    /// Verifies that an OTLP request with no resource spans returns a 500 error.
    /// Previously, `size_of_val(&traces)` always returned 24 (Vec stack size),
    /// so the empty check never fired.
    #[tokio::test]
    async fn test_v1_traces_empty_request_returns_error() {
        let processor = Arc::new(NoopTraceProcessor::new());
        let router = make_router_with_processor(processor);

        let body = encode_otlp_request(&ExportTraceServiceRequest { resource_spans: vec![] });
        let request = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header("content-type", "application/x-protobuf")
            .body(Body::from(body))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// Verifies that `body_size` passed to `process_traces` equals the raw OTLP
    /// request body length, not `size_of_val(&traces)` (which was always 24 bytes).
    #[tokio::test]
    async fn test_v1_traces_body_size_equals_request_body_len() {
        let processor = Arc::new(NoopTraceProcessor::new());
        let router = make_router_with_processor(Arc::clone(&processor) as Arc<dyn TraceProcessor + Send + Sync>);

        use opentelemetry_proto::tonic::common::v1::InstrumentationScope;
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
        let otlp_request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource::default()),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope::default()),
                    spans: vec![Span {
                        name: "test-span".to_string(),
                        trace_id: vec![1u8; 16],
                        span_id: vec![1u8; 8],
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        };
        let body = encode_otlp_request(&otlp_request);
        let expected_body_size = body.len();

        let request = Request::builder()
            .method("POST")
            .uri("/v1/traces")
            .header("content-type", "application/x-protobuf")
            .body(Body::from(body))
            .unwrap();

        router.oneshot(request).await.unwrap();
        assert_eq!(
            processor.captured_body_size(),
            Some(expected_body_size),
            "body_size must equal the raw OTLP request body length"
        );
    }

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
