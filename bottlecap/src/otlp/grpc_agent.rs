use libdd_trace_utils::trace_utils::TracerHeaderTags as DatadogTracerHeaderTags;
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
    trace_service_server::{TraceService, TraceServiceServer},
};
use std::mem::size_of_val;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{debug, error};

use crate::{
    config::Config,
    otlp::processor::Processor as OtlpProcessor,
    tags::provider,
    traces::{
        stats_generator::StatsGenerator, trace_aggregator::SendDataBuilderInfo,
        trace_processor::TraceProcessor,
    },
};

const OTLP_AGENT_GRPC_PORT: u16 = 4317;
const DEFAULT_MAX_RECV_MSG_SIZE: usize = 4 * 1024 * 1024; // 4MB default

struct OtlpGrpcService {
    config: Arc<Config>,
    tags_provider: Arc<provider::Provider>,
    processor: OtlpProcessor,
    trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendDataBuilderInfo>,
    stats_generator: Arc<StatsGenerator>,
}

#[tonic::async_trait]
impl TraceService for OtlpGrpcService {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let inner_request = request.into_inner();

        let traces = match self.processor.process_request(inner_request) {
            Ok(traces) => traces,
            Err(e) => {
                error!("OTLP gRPC | Failed to process request: {:?}", e);
                return Err(Status::internal(format!("Failed to process request: {e}")));
            }
        };

        let tracer_header_tags = DatadogTracerHeaderTags::default();
        let body_size = size_of_val(&traces);
        if body_size == 0 {
            error!("OTLP gRPC | Not sending traces, processor returned empty data");
            return Err(Status::internal(
                "Not sending traces, processor returned empty data",
            ));
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
                error!("OTLP gRPC | Error sending traces to the trace aggregator: {err}");
                return Err(Status::internal(format!(
                    "Error sending traces to the trace aggregator: {err}"
                )));
            }
            debug!("OTLP gRPC | Successfully buffered traces to be aggregated.");
        }

        // Compute trace stats after process_traces() which performs obfuscation
        if compute_trace_stats_on_extension
            && let Err(err) = self.stats_generator.send(&processed_traces)
        {
            // Just log the error. Stats are not critical.
            error!("OTLP gRPC | Error sending traces to the stats concentrator: {err}");
        }

        Ok(Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

pub struct GrpcAgent {
    config: Arc<Config>,
    tags_provider: Arc<provider::Provider>,
    processor: OtlpProcessor,
    trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendDataBuilderInfo>,
    stats_generator: Arc<StatsGenerator>,
    port: u16,
    cancel_token: CancellationToken,
}

impl GrpcAgent {
    pub fn new(
        config: Arc<Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendDataBuilderInfo>,
        stats_generator: Arc<StatsGenerator>,
    ) -> Self {
        let port = Self::parse_port(
            config.otlp_config_receiver_protocols_grpc_endpoint.as_ref(),
            OTLP_AGENT_GRPC_PORT,
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
                    error!("Invalid OTLP gRPC port, using default port {default_port}");
                    default_port
                });
            }

            error!("Invalid OTLP gRPC endpoint format, using default port {default_port}");
        }

        default_port
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let socket = SocketAddr::from(([127, 0, 0, 1], self.port));

        let max_recv_msg_size = self
            .config
            .otlp_config_receiver_protocols_grpc_max_recv_msg_size_mib
            .map_or(DEFAULT_MAX_RECV_MSG_SIZE, |mib| {
                mib.unsigned_abs() as usize * 1024 * 1024
            });

        let service = OtlpGrpcService {
            config: Arc::clone(&self.config),
            tags_provider: Arc::clone(&self.tags_provider),
            processor: self.processor.clone(),
            trace_processor: Arc::clone(&self.trace_processor),
            trace_tx: self.trace_tx.clone(),
            stats_generator: Arc::clone(&self.stats_generator),
        };

        let cancel_token = self.cancel_token.clone();

        debug!(
            "OTLP gRPC | Starting collector on {} with max message size {} bytes",
            socket, max_recv_msg_size
        );

        tonic::transport::Server::builder()
            .add_service(
                TraceServiceServer::new(service).max_decoding_message_size(max_recv_msg_size),
            )
            .serve_with_shutdown(socket, async move {
                cancel_token.cancelled().await;
                debug!("OTLP gRPC | Shutdown signal received, shutting down");
            })
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_port_with_valid_endpoint() {
        let endpoint = Some("localhost:4317".to_string());
        assert_eq!(
            GrpcAgent::parse_port(endpoint.as_ref(), OTLP_AGENT_GRPC_PORT),
            4317
        );
    }

    #[test]
    fn test_parse_port_with_custom_port() {
        let endpoint = Some("0.0.0.0:9999".to_string());
        assert_eq!(
            GrpcAgent::parse_port(endpoint.as_ref(), OTLP_AGENT_GRPC_PORT),
            9999
        );
    }

    #[test]
    fn test_parse_port_with_invalid_port_format() {
        let endpoint = Some("localhost:invalid".to_string());
        assert_eq!(
            GrpcAgent::parse_port(endpoint.as_ref(), OTLP_AGENT_GRPC_PORT),
            OTLP_AGENT_GRPC_PORT
        );
    }

    #[test]
    fn test_parse_port_with_missing_port() {
        let endpoint = Some("localhost".to_string());
        assert_eq!(
            GrpcAgent::parse_port(endpoint.as_ref(), OTLP_AGENT_GRPC_PORT),
            OTLP_AGENT_GRPC_PORT
        );
    }

    #[test]
    fn test_parse_port_with_none_endpoint() {
        let endpoint: Option<String> = None;
        assert_eq!(
            GrpcAgent::parse_port(endpoint.as_ref(), OTLP_AGENT_GRPC_PORT),
            OTLP_AGENT_GRPC_PORT
        );
    }
}
