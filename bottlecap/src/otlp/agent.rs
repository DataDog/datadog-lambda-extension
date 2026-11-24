use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
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
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc::Sender},
};
use tokio_util::sync::CancellationToken;
use tonic_types::Status as ProtoStatus;
use tracing::{debug, error};

use crate::{
    config::Config,
    http::{extract_request_body, handler_not_found},
    otlp::{
        processor::{OtlpEncoding, Processor as OtlpProcessor},
        should_enable_otlp_metrics, should_enable_otlp_traces,
    },
    tags::provider,
    traces::{
        proxy_aggregator::{self, ProxyRequest},
        stats_generator::StatsGenerator,
        trace_aggregator::SendDataBuilderInfo,
        trace_processor::{StatsComputedBy, TraceProcessor},
    },
};

const OTLP_AGENT_HTTP_PORT: u16 = 4318;
const OTLP_AGENT_GRPC_PORT: u16 = 4317;
const DEFAULT_MAX_RECV_MSG_SIZE: usize = 4 * 1024 * 1024; // 4MB default
const MAX_RECV_MSG_SIZE_CAP: usize = 50 * 1024 * 1024; // 50MB cap (matches trace agent)

const OTLP_METRICS_PATH: &str = "/v1/metrics";
const DD_OTEL_METRIC_CONFIG_HEADER: &str = "dd-otel-metric-config";
// The Go agent's OTLP defaults: instrumentation scope tags on, and other resource attributes
// (runtime-id, DD_TAGS) as tags only with DD_OTLP_CONFIG_METRICS_RESOURCE_ATTRIBUTES_AS_TAGS.
const DD_OTEL_METRIC_CONFIG: &str =
    r#"{"resource_attributes_as_tags": false, "instrumentation_scope_metadata_as_tags": true}"#;
const DD_OTEL_METRIC_CONFIG_RESOURCE_ATTRIBUTES_AS_TAGS: &str =
    r#"{"resource_attributes_as_tags": true, "instrumentation_scope_metadata_as_tags": true}"#;

/// State for `/v1/metrics`: requests are queued unchanged and sent to Datadog's OTLP intake
/// by the proxy flusher.
#[derive(Clone)]
struct MetricsProxy {
    config: Arc<Config>,
    proxy_aggregator: Arc<Mutex<proxy_aggregator::Aggregator>>,
}

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

        let lambda_extension_compute_stats = self.config.ext.lambda_extension_compute_stats;
        // Capture before `tracer_header_tags` is moved into process_traces below.
        let client_computed_stats = tracer_header_tags.generic.client_computed_stats;
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
        // Skip extension-side stats generation when the tracer already computed stats
        // client-side (Datadog-Client-Computed-Stats), to avoid double-counting.
        if StatsComputedBy::resolve(lambda_extension_compute_stats, client_computed_stats)
            == StatsComputedBy::Extension
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

        // Default tags mean client_computed_stats is always false for OTLP gRPC, so
        // Datadog-Client-Computed-Stats metadata is ignored. Safe today: Datadog tracers
        // that compute stats never export via OTLP. Revisit if that ever changes, to
        // avoid double-counting stats.
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
    metrics_proxy: Option<MetricsProxy>,
    traces_enabled: bool,
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
        proxy_aggregator: Arc<Mutex<proxy_aggregator::Aggregator>>,
        cancel_token: CancellationToken,
    ) -> Self {
        let traces_enabled = should_enable_otlp_traces(&config);
        let metrics_enabled = should_enable_otlp_metrics(&config);

        let http_endpoint = config.otlp_config_receiver_protocols_http_endpoint.as_ref();
        let http_port = (metrics_enabled || (traces_enabled && http_endpoint.is_some()))
            .then(|| Self::parse_port(http_endpoint, OTLP_AGENT_HTTP_PORT));

        let grpc_port = config
            .otlp_config_receiver_protocols_grpc_endpoint
            .as_ref()
            .filter(|_| traces_enabled)
            .map(|ep| Self::parse_port(Some(ep), OTLP_AGENT_GRPC_PORT));

        let metrics_proxy = metrics_enabled.then(|| MetricsProxy {
            config: Arc::clone(&config),
            proxy_aggregator,
        });

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
            metrics_proxy,
            traces_enabled,
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
            let router = Self::http_router(
                self.traces_enabled.then(|| self.pipeline.clone()),
                self.metrics_proxy.clone(),
            );
            let cancel_token = self.cancel_token.clone();

            handles.push(tokio::spawn(async move {
                if let Err(e) = Self::start_http(port, router, cancel_token).await {
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

    fn http_router(pipeline: Option<TracePipeline>, metrics_proxy: Option<MetricsProxy>) -> Router {
        let mut router = Router::new();
        if let Some(pipeline) = pipeline {
            router = router.merge(
                Router::new()
                    .route("/v1/traces", post(Self::v1_traces))
                    .with_state(pipeline),
            );
        }
        if let Some(metrics_proxy) = metrics_proxy {
            router = router.merge(
                Router::new()
                    .route(OTLP_METRICS_PATH, post(Self::v1_metrics))
                    .with_state(metrics_proxy),
            );
        }
        router.fallback(handler_not_found)
    }

    async fn start_http(
        port: u16,
        router: Router,
        cancel_token: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let socket = SocketAddr::from(([127, 0, 0, 1], port));

        let listener = TcpListener::bind(&socket).await?;
        debug!("OTLP HTTP | Starting collector on {}", socket);
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                cancel_token.cancelled().await;
                debug!("OTLP HTTP | Shutdown signal received, shutting down");
            })
            .await?;

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
        let body_size = body.len();

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

    async fn v1_metrics(State(metrics_proxy): State<MetricsProxy>, request: Request) -> Response {
        let encoding = OtlpEncoding::from_content_type(
            request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
        );

        let (mut parts, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                error!("OTLP HTTP | Failed to extract metrics request body: {e}");
                return Self::otlp_error_response(
                    StatusCode::BAD_REQUEST,
                    format!("Failed to extract request body: {e}"),
                    encoding,
                );
            }
        };

        let metric_config = if metrics_proxy
            .config
            .otlp_config_metrics_resource_attributes_as_tags
        {
            DD_OTEL_METRIC_CONFIG_RESOURCE_ATTRIBUTES_AS_TAGS
        } else {
            DD_OTEL_METRIC_CONFIG
        };
        parts.headers.insert(
            DD_OTEL_METRIC_CONFIG_HEADER,
            HeaderValue::from_static(metric_config),
        );
        let proxy_request = ProxyRequest {
            headers: parts.headers,
            body,
            target_url: format!(
                "https://otlp.{}{OTLP_METRICS_PATH}",
                metrics_proxy.config.site
            ),
        };
        metrics_proxy
            .proxy_aggregator
            .lock()
            .await
            .add(proxy_request);
        debug!("OTLP HTTP | Queued metrics for the OTLP intake");

        // An empty trace export response encodes the same as an empty metrics one.
        Self::otlp_success_response(encoding)
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use axum::body::Body;
    use tower::ServiceExt;

    fn metrics_proxy() -> MetricsProxy {
        MetricsProxy {
            config: Arc::new(Config {
                site: "datadoghq.eu".to_string(),
                ..Config::default()
            }),
            proxy_aggregator: Arc::new(Mutex::new(proxy_aggregator::Aggregator::default())),
        }
    }

    fn post_request(path: &str, content_type: &str, body: &'static [u8]) -> Request {
        Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn test_v1_metrics_queues_request_for_otlp_intake() {
        let metrics_proxy = metrics_proxy();
        let router = Agent::http_router(None, Some(metrics_proxy.clone()));

        let response = router
            .oneshot(post_request(
                "/v1/metrics",
                "application/x-protobuf",
                b"\x0a\x02\x08\x01",
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-protobuf"
        );

        let batch = metrics_proxy.proxy_aggregator.lock().await.get_batch();
        assert_eq!(batch.len(), 1);
        let request = batch.first().unwrap();
        assert_eq!(request.target_url, "https://otlp.datadoghq.eu/v1/metrics");
        assert_eq!(request.body.as_ref(), b"\x0a\x02\x08\x01");
        assert_eq!(
            request.headers.get(header::CONTENT_TYPE).unwrap(),
            "application/x-protobuf"
        );
        assert_eq!(
            request.headers.get(DD_OTEL_METRIC_CONFIG_HEADER).unwrap(),
            DD_OTEL_METRIC_CONFIG
        );
    }

    #[tokio::test]
    async fn test_v1_metrics_sends_resource_attributes_as_tags_when_configured() {
        let metrics_proxy = MetricsProxy {
            config: Arc::new(Config {
                otlp_config_metrics_resource_attributes_as_tags: true,
                ..Config::default()
            }),
            proxy_aggregator: Arc::new(Mutex::new(proxy_aggregator::Aggregator::default())),
        };
        let router = Agent::http_router(None, Some(metrics_proxy.clone()));

        let response = router
            .oneshot(post_request("/v1/metrics", "application/x-protobuf", b""))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let batch = metrics_proxy.proxy_aggregator.lock().await.get_batch();
        assert_eq!(
            batch
                .first()
                .unwrap()
                .headers
                .get(DD_OTEL_METRIC_CONFIG_HEADER)
                .unwrap(),
            DD_OTEL_METRIC_CONFIG_RESOURCE_ATTRIBUTES_AS_TAGS
        );
    }

    #[tokio::test]
    async fn test_v1_metrics_replies_json_for_json_request() {
        let router = Agent::http_router(None, Some(metrics_proxy()));

        let response = router
            .oneshot(post_request("/v1/metrics", "application/json", b"{}"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[tokio::test]
    async fn test_v1_metrics_not_found_when_metrics_disabled() {
        let router = Agent::http_router(None, None);

        let response = router
            .oneshot(post_request("/v1/metrics", "application/x-protobuf", b""))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_v1_traces_not_found_when_only_metrics_enabled() {
        let metrics_proxy = metrics_proxy();
        let router = Agent::http_router(None, Some(metrics_proxy.clone()));

        let response = router
            .oneshot(post_request("/v1/traces", "application/x-protobuf", b""))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            metrics_proxy
                .proxy_aggregator
                .lock()
                .await
                .get_batch()
                .is_empty()
        );
    }

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
