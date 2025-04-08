use datadog_trace_utils::send_data::SendData;
use datadog_trace_utils::trace_utils::TracerHeaderTags as DatadogTracerHeaderTags;
use ddcommon::hyper_migration;
use http_body_util::BodyExt;
use hyper::{http, service::service_fn, Method, Response, StatusCode};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use trace_agent::http_utils::{
    log_and_create_http_response, log_and_create_traces_success_http_response,
};
use tracing::{debug, error};

use crate::{
    config::Config, otlp::processor::Processor as OtlpProcessor, tags::provider,
    traces::trace_processor::TraceProcessor,
};

const OTLP_AGENT_HTTP_PORT: u16 = 4318;

pub struct Agent {
    pub config: Arc<Config>,
    pub tags_provider: Arc<provider::Provider>,
    pub processor: OtlpProcessor,
    pub trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
    pub trace_tx: Sender<SendData>,
    port: u16,
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

        Self {
            config: config.clone(),
            tags_provider: tags_provider.clone(),
            processor: OtlpProcessor::new(config.clone()),
            trace_processor,
            trace_tx,
            port,
        }
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

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config = self.config.clone();
        let tags_provider = self.tags_provider.clone();
        let processor = self.processor.clone();
        let trace_tx = self.trace_tx.clone();
        let trace_processor = self.trace_processor.clone();
        let service = service_fn(move |req| {
            Self::handler(
                req.map(hyper_migration::Body::incoming),
                config.clone(),
                tags_provider.clone(),
                processor.clone(),
                trace_processor.clone(),
                trace_tx.clone(),
            )
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        let server = hyper::server::conn::http1::Builder::new();
        let mut joinset = tokio::task::JoinSet::new();
        debug!("OTLP started on {}", addr);
        loop {
            let conn = tokio::select! {
                con_res = listener.accept() => match con_res {
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionRefused
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => {
                        error!("OTLP Receiver error: {e}");
                        return Err(e.into());
                    }
                    Ok((conn, _)) => conn,
                },
                finished = async {
                    match joinset.join_next().await {
                        Some(finished) => finished,
                        None => std::future::pending().await,
                    }
                } => match finished {
                    Err(e) if e.is_panic() => {
                        std::panic::resume_unwind(e.into_panic());
                    },
                    Ok(()) | Err(_) => continue,
                },
            };
            let conn = hyper_util::rt::TokioIo::new(conn);
            let server = server.clone();
            let service = service.clone();
            joinset.spawn(async move {
                if let Err(e) = server.serve_connection(conn, service).await {
                    error!("OTLP Receiver connection error: {e}");
                }
            });
        }
    }

    #[allow(clippy::pedantic)]
    async fn handler(
        req: hyper_migration::HttpRequest,
        config: Arc<Config>,
        tags_provider: Arc<provider::Provider>,
        processor: OtlpProcessor,
        trace_processor: Arc<dyn TraceProcessor + Send + Sync>,
        trace_tx: Sender<SendData>,
    ) -> http::Result<hyper_migration::HttpResponse> {
        match (req.method(), req.uri().path()) {
            (&Method::POST, "/v1/traces") => {
                let headers = req.headers().clone();
                let body = match req.collect().await {
                    Ok(body_bytes_collected) => body_bytes_collected.to_bytes().to_vec(),
                    Err(e) => {
                        error!("Failed to collect body: {:?}", e);
                        return Ok(Response::builder()
                            .status(hyper::StatusCode::INTERNAL_SERVER_ERROR)
                            .body(hyper_migration::Body::from("Failed to collect body"))
                            .expect("infallible"));
                    }
                };

                let traces = match processor.process(&body) {
                    Ok(traces) => traces,
                    Err(e) => {
                        error!("Failed to process OTLP request: {:?}", e);
                        return Ok(Response::builder()
                            .status(hyper::StatusCode::INTERNAL_SERVER_ERROR)
                            .body(hyper_migration::Body::from(
                                "Failed to process OTLP request",
                            ))
                            .expect("infallible"));
                    }
                };

                let tracer_header_tags: DatadogTracerHeaderTags = (&headers).into();

                let body_size = size_of_val(&traces);
                let send_data = trace_processor.process_traces(
                    config,
                    tags_provider.clone(),
                    tracer_header_tags,
                    traces,
                    body_size,
                    None,
                );

                if send_data.is_empty() {
                    return log_and_create_http_response(
                        "Not sending traces, processor returned empty data",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }

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
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
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
