use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use libdd_trace_protobuf::pb::Span as DatadogSpan;
use libdd_trace_utils::trace_utils::TracerHeaderTags as DatadogTracerHeaderTags;
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
    trace_service_server::{TraceService, TraceServiceServer},
};
use prost::Message;
use std::mem::size_of_val;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tonic_types::Status as ProtoStatus;
use tracing::{debug, error};

use crate::{
    config::Config,
    http::{extract_request_body, handler_not_found},
    otlp::processor::{OtlpEncoding, Processor as OtlpProcessor},
    tags::provider,
    traces::{
        stats_generator::StatsGenerator, trace_aggregator::SendDataBuilderInfo,
        trace_processor::TraceProcessor,
    },
};

const OTLP_AGENT_HTTP_PORT: u16 = 4318;
const OTLP_AGENT_GRPC_PORT: u16 = 4317;
const DEFAULT_MAX_RECV_MSG_SIZE: usize = 4 * 1024 * 1024; // 4MB default
const MAX_RECV_MSG_SIZE_CAP: usize = 50 * 1024 * 1024; // 50MB cap (matches trace agent)

/// Shared state and trace processing pipeline used by both HTTP and gRPC handlers.
#[derive(Clone)]
struct TracePipeline {
    config: Arc<Config>,
    tags_provider: Arc<provider::Provider>,
    processor: OtlpProcessor,
    trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendDataBuilderInfo>,
    stats_generator: Arc<StatsGenerator>,
}

impl TracePipeline {
    async fn process_and_send_traces(
        &self,
        tracer_header_tags: DatadogTracerHeaderTags<'_>,
        traces: Vec<Vec<DatadogSpan>>,
        body_size: usize,
    ) -> Result<(), String> {
        if traces.iter().all(Vec::is_empty) {
            return Err("Not sending traces, processor returned empty data".to_string());
        }

        let compute_trace_stats_on_extension = self.config.compute_trace_stats_on_extension;
        let (send_data_builder, processed_traces) = self.trace_processor.process_traces(
            self.config.clone(),
            self.tags_provider.clone(),
            tracer_header_tags,
            traces,
            body_size,
            None,
        );

        if let Some(send_data_builder) = send_data_builder {
            if let Err(err) = self.trace_tx.send(send_data_builder).await {
                return Err(format!(
                    "Error sending traces to the trace aggregator: {err}"
                ));
            }
            debug!("OTLP | Successfully buffered traces to be aggregated.");
        }

        // This needs to be after process_traces() because process_traces()
        // performs obfuscation, and we need to compute stats on the obfuscated traces.
        if compute_trace_stats_on_extension
            && let Err(err) = self.stats_generator.send(&processed_traces)
        {
            // Just log the error. We don't think trace stats are critical, so we don't want to
            // return an error if only stats fail to send.
            error!("OTLP | Error sending traces to the stats concentrator: {err}");
        }

        Ok(())
    }
}

// --- gRPC: implement TraceService directly on TracePipeline ---

#[tonic::async_trait]
impl TraceService for TracePipeline {
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        let inner_request = request.into_inner();
        let body_size = inner_request.encoded_len();

        let traces = match self.processor.process_request(inner_request) {
            Ok(traces) => traces,
            Err(e) => {
                error!("OTLP gRPC | Failed to process request: {:?}", e);
                return Err(tonic::Status::internal(format!(
                    "Failed to process request: {e}"
                )));
            }
        };

        let tracer_header_tags = DatadogTracerHeaderTags::default();

        self.process_and_send_traces(tracer_header_tags, traces, body_size)
            .await
            .map_err(|msg| {
                error!("OTLP gRPC | {msg}");
                tonic::Status::internal(msg)
            })?;

        Ok(tonic::Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

// --- Agent ---

pub struct Agent {
    pipeline: TracePipeline,
    cancel_token: CancellationToken,
    http_port: Option<u16>,
    grpc_port: Option<u16>,
}

impl Agent {
    pub fn new(
        config: Arc<Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendDataBuilderInfo>,
        stats_generator: Arc<StatsGenerator>,
        cancel_token: CancellationToken,
    ) -> Self {
        let http_port = config
            .otlp_config_receiver_protocols_http_endpoint
            .as_ref()
            .map(|ep| Self::parse_port(Some(ep), OTLP_AGENT_HTTP_PORT));

        let grpc_port = config
            .otlp_config_receiver_protocols_grpc_endpoint
            .as_ref()
            .map(|ep| Self::parse_port(Some(ep), OTLP_AGENT_GRPC_PORT));

        let pipeline = TracePipeline {
            config: Arc::clone(&config),
            tags_provider: Arc::clone(&tags_provider),
            processor: OtlpProcessor::new(Arc::clone(&config)),
            trace_processor,
            trace_tx,
            stats_generator,
        };

        Self {
            pipeline,
            cancel_token,
            http_port,
            grpc_port,
        }
    }

    fn parse_port(endpoint: Option<&String>, default_port: u16) -> u16 {
        if let Some(endpoint) = endpoint {
            let without_scheme = endpoint
                .strip_prefix("http://")
                .or_else(|| endpoint.strip_prefix("https://"))
                .unwrap_or(endpoint);

            // rsplit handles IPv6 like [::1]:4317
            if let Some(port_str) = without_scheme.rsplit(':').next()
                && let Ok(port) = port_str.parse::<u16>()
            {
                return port;
            }

            error!(
                "Invalid OTLP endpoint format '{}', using default port {}",
                endpoint, default_port
            );
        }

        default_port
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        if let Some(port) = self.http_port {
            let pipeline = self.pipeline.clone();
            let cancel_token = self.cancel_token.clone();

            handles.push(tokio::spawn(async move {
                if let Err(e) = Self::start_http(port, pipeline, cancel_token).await {
                    error!("OTLP HTTP agent error: {e:?}");
                }
            }));
        }

        if let Some(port) = self.grpc_port {
            let pipeline = self.pipeline.clone();
            let cancel_token = self.cancel_token.clone();
            let max_recv_msg_size = Self::resolve_grpc_max_msg_size(&self.pipeline.config);

            handles.push(tokio::spawn(async move {
                if let Err(e) =
                    Self::start_grpc(port, pipeline, cancel_token, max_recv_msg_size).await
                {
                    error!("OTLP gRPC agent error: {e:?}");
                }
            }));
        }

        for handle in handles {
            if let Err(e) = handle.await {
                error!("OTLP agent task panicked: {e:?}");
            }
        }

        Ok(())
    }

    async fn start_http(
        port: u16,
        pipeline: TracePipeline,
        cancel_token: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let socket = SocketAddr::from(([127, 0, 0, 1], port));
        let router = Router::new()
            .route("/v1/traces", post(Self::v1_traces))
            .fallback(handler_not_found)
            .with_state(pipeline);

        let listener = TcpListener::bind(&socket)
            .await
            .expect("Failed to bind HTTP socket");
        debug!("OTLP HTTP | Starting collector on {}", socket);
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                cancel_token.cancelled().await;
                debug!("OTLP HTTP | Shutdown signal received, shutting down");
            })
            .await
            .expect("Failed to start OTLP HTTP agent");

        Ok(())
    }

    async fn start_grpc(
        port: u16,
        pipeline: TracePipeline,
        cancel_token: CancellationToken,
        max_recv_msg_size: usize,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let socket = SocketAddr::from(([127, 0, 0, 1], port));

        debug!(
            "OTLP gRPC | Starting collector on {} with max message size {} bytes",
            socket, max_recv_msg_size
        );

        tonic::transport::Server::builder()
            .add_service(
                TraceServiceServer::new(pipeline).max_decoding_message_size(max_recv_msg_size),
            )
            .serve_with_shutdown(socket, async move {
                cancel_token.cancelled().await;
                debug!("OTLP gRPC | Shutdown signal received, shutting down");
            })
            .await?;

        Ok(())
    }

    fn resolve_grpc_max_msg_size(config: &Config) -> usize {
        config
            .otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
            .map_or(DEFAULT_MAX_RECV_MSG_SIZE, |mib| {
                if mib <= 0 {
                    error!(
                        "Invalid gRPC max message size {}MiB, using default {}MiB",
                        mib,
                        DEFAULT_MAX_RECV_MSG_SIZE / (1024 * 1024)
                    );
                    return DEFAULT_MAX_RECV_MSG_SIZE;
                }
                // Safe: we validated mib > 0 above
                #[allow(clippy::cast_sign_loss)]
                let size = (mib as usize) * 1024 * 1024;
                if size > MAX_RECV_MSG_SIZE_CAP {
                    error!(
                        "gRPC max message size {}MiB exceeds cap, limiting to {}MiB",
                        mib,
                        MAX_RECV_MSG_SIZE_CAP / (1024 * 1024)
                    );
                    return MAX_RECV_MSG_SIZE_CAP;
                }
                size
            })
    }

    // --- HTTP handler ---

    async fn v1_traces(State(pipeline): State<TracePipeline>, request: Request) -> Response {
        let content_type = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let encoding = OtlpEncoding::from_content_type(content_type.as_deref());

        let (parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                error!("OTLP HTTP | Failed to extract request body: {e}");
                return Self::otlp_error_response(
                    StatusCode::BAD_REQUEST,
                    format!("Failed to extract request body: {e}"),
                    encoding,
                );
            }
        };

        let traces = match pipeline.processor.process(&body, encoding) {
            Ok(traces) => traces,
            Err(e) => {
                error!("OTLP HTTP | Failed to process request: {:?}", e);
                return Self::otlp_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to process request: {e}"),
                    encoding,
                );
            }
        };

        let tracer_header_tags: DatadogTracerHeaderTags = (&parts.headers).into();
        let body_size = size_of_val(&traces);

        match pipeline
            .process_and_send_traces(tracer_header_tags, traces, body_size)
            .await
        {
            Ok(()) => Self::otlp_success_response(encoding),
            Err(msg) => {
                error!("OTLP HTTP | {msg}");
                Self::otlp_error_response(StatusCode::INTERNAL_SERVER_ERROR, msg, encoding)
            }
        }
    }

    fn otlp_error_response(
        status_code: StatusCode,
        message: String,
        encoding: OtlpEncoding,
    ) -> Response {
        let body = match encoding {
            OtlpEncoding::Json => {
                let json_error = serde_json::json!({
                    "code": status_code.as_u16(),
                    "message": message
                });
                match serde_json::to_vec(&json_error) {
                    Ok(buf) => buf,
                    Err(e) => {
                        error!("OTLP | Failed to encode error response as JSON: {e}");
                        return (status_code, [(header::CONTENT_TYPE, "text/plain")], message)
                            .into_response();
                    }
                }
            }
            OtlpEncoding::Protobuf => {
                let status = ProtoStatus {
                    code: i32::from(status_code.as_u16()),
                    message: message.clone(),
                    details: vec![],
                };
                let mut buf = Vec::new();
                if let Err(e) = status.encode(&mut buf) {
                    error!("OTLP | Failed to encode error response as Protobuf: {e}");
                    return (status_code, [(header::CONTENT_TYPE, "text/plain")], message)
                        .into_response();
                }
                buf
            }
        };

        (
            status_code,
            [(header::CONTENT_TYPE, encoding.content_type())],
            body,
        )
            .into_response()
    }

    fn otlp_success_response(encoding: OtlpEncoding) -> Response {
        let response = ExportTraceServiceResponse {
            partial_success: None,
        };

        let body = match encoding {
            OtlpEncoding::Json => match serde_json::to_vec(&response) {
                Ok(buf) => buf,
                Err(e) => {
                    error!("OTLP | Failed to encode success response as JSON: {e}");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to encode response".to_string(),
                    )
                        .into_response();
                }
            },
            OtlpEncoding::Protobuf => {
                let mut buf = Vec::new();
                if let Err(e) = response.encode(&mut buf) {
                    error!("OTLP | Failed to encode success response as Protobuf: {e}");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Failed to encode response".to_string(),
                    )
                        .into_response();
                }
                buf
            }
        };

        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, encoding.content_type())],
            body,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port_with_valid_endpoint() {
        let endpoint = Some("localhost:8080".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            8080
        );
    }

    #[test]
    fn test_parse_port_with_custom_port() {
        let endpoint = Some("0.0.0.0:9999".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            9999
        );
    }

    #[test]
    fn test_parse_port_with_http_scheme() {
        let endpoint = Some("http://localhost:4318".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            4318
        );
    }

    #[test]
    fn test_parse_port_with_https_scheme() {
        let endpoint = Some("https://localhost:4317".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_GRPC_PORT),
            4317
        );
    }

    #[test]
    fn test_parse_port_with_invalid_port_format() {
        let endpoint = Some("localhost:invalid".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_missing_port() {
        let endpoint = Some("localhost".to_string());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_none_endpoint() {
        let endpoint: Option<String> = None;
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }

    #[test]
    fn test_parse_port_with_empty_endpoint() {
        let endpoint = Some(String::new());
        assert_eq!(
            Agent::parse_port(endpoint.as_ref(), OTLP_AGENT_HTTP_PORT),
            OTLP_AGENT_HTTP_PORT
        );
    }
}
