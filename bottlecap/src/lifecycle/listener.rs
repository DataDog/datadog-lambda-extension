// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::hyper_migration;
use ddcommon::hyper_migration::Body;
use http_body_util::BodyExt;
use hyper::service::service_fn;
use hyper::{http, HeaderMap, Method, Response, StatusCode};
use serde_json::json;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

use crate::lifecycle::invocation::processor::Processor as InvocationProcessor;
use crate::traces::propagation::text_map_propagator::{
    DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY, DATADOG_SAMPLING_PRIORITY_KEY, DATADOG_TAGS_KEY,
    DATADOG_TRACE_ID_KEY,
};

const HELLO_PATH: &str = "/lambda/hello";
const START_INVOCATION_PATH: &str = "/lambda/start-invocation";
const END_INVOCATION_PATH: &str = "/lambda/end-invocation";
const AGENT_PORT: usize = 8124;

pub struct Listener {
    pub invocation_processor: Arc<Mutex<InvocationProcessor>>,
}

impl Listener {
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let invocation_processor = self.invocation_processor.clone();

        let service = service_fn(move |req| {
            let invocation_processor = invocation_processor.clone();

            Self::handler(
                req.map(hyper_migration::Body::incoming),
                invocation_processor.clone(),
            )
        });

        let port = u16::try_from(AGENT_PORT).expect("AGENT_PORT is too large");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        let server = hyper::server::conn::http1::Builder::new();
        let mut joinset = tokio::task::JoinSet::new();
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
                        error!("Server error: {e}");
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
                    error!("Connection error: {e}");
                }
            });
        }
    }

    async fn handler(
        req: hyper_migration::HttpRequest,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> http::Result<hyper_migration::HttpResponse> {
        match (req.method(), req.uri().path()) {
            (&Method::POST, START_INVOCATION_PATH) => {
                let (parts, body) = req.into_parts();
                Self::universal_instrumentation_start(&parts.headers, body, invocation_processor)
                    .await
                    .1
            }
            (&Method::POST, END_INVOCATION_PATH) => {
                let (parts, body) = req.into_parts();
                match Self::universal_instrumentation_end(
                    &parts.headers,
                    body,
                    invocation_processor,
                )
                .await
                {
                    Ok(response) => Ok(response),
                    Err(e) => {
                        error!("Failed to end invocation {e}");
                        Ok(Response::builder()
                            .status(500)
                            .body(hyper_migration::Body::empty())
                            .expect("no body"))
                    }
                }
            }
            (&Method::GET, HELLO_PATH) => Self::hello_handler(),
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }

    pub async fn universal_instrumentation_start(
        headers: &HeaderMap,
        body: Body,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> (u64, http::Result<hyper_migration::HttpResponse>) {
        debug!("Received start invocation request");
        match body.collect().await {
            Ok(b) => {
                let body = b.to_bytes().to_vec();

                let headers = Self::headers_to_map(headers);

                let extracted_span_context = {
                    let mut processor = invocation_processor.lock().await;
                    processor.on_universal_instrumentation_start(headers, body)
                };
                let mut response = Response::builder().status(200);

                let found_parent_span_id;
                // If a `SpanContext` exists, then tell the tracer to use it.
                // todo: update this whole code with DatadogHeaderPropagator::inject
                // since this logic looks messy
                if let Some(sp) = extracted_span_context {
                    response = response.header(DATADOG_TRACE_ID_KEY, sp.trace_id.to_string());
                    if let Some(priority) = sp.sampling.and_then(|s| s.priority) {
                        response =
                            response.header(DATADOG_SAMPLING_PRIORITY_KEY, priority.to_string());
                    }

                    // Handle 128 bit trace ids
                    if let Some(trace_id_higher_order_bits) =
                        sp.tags.get(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY)
                    {
                        response = response.header(
                        DATADOG_TAGS_KEY,
                        format!("{DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY}={trace_id_higher_order_bits}"),
                    );
                    }
                    found_parent_span_id = sp.span_id;
                } else {
                    found_parent_span_id = 0;
                }

                (
                    found_parent_span_id,
                    response.body(hyper_migration::Body::from(json!({}).to_string())),
                )
            }
            Err(e) => {
                error!("Could not read start invocation request body {e}");

                (
                    0,
                    Response::builder()
                        .status(400)
                        .body(hyper_migration::Body::from(
                            "Could not read start invocation request body",
                        )),
                )
            }
        }
    }

    pub async fn universal_instrumentation_end(
        headers: &HeaderMap,
        body: Body,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> http::Result<hyper_migration::HttpResponse> {
        debug!("Received end invocation request");
        match body.collect().await {
            Ok(b) => {
                let body = b.to_bytes().to_vec();
                let mut processor = invocation_processor.lock().await;

                let headers = Self::headers_to_map(headers);
                processor.on_universal_instrumentation_end(headers, body);
                drop(processor);

                Response::builder()
                    .status(200)
                    .body(hyper_migration::Body::from(json!({}).to_string()))
            }
            Err(e) => {
                error!("Could not read end invocation request body {e}");

                Response::builder()
                    .status(400)
                    .body(hyper_migration::Body::from(
                        "Could not read end invocation request body",
                    ))
            }
        }
    }

    fn hello_handler() -> http::Result<hyper_migration::HttpResponse> {
        warn!("[DEPRECATED] Please upgrade your tracing library, the /hello route is deprecated");
        Response::builder()
            .status(200)
            .body(hyper_migration::Body::from(json!({}).to_string()))
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
