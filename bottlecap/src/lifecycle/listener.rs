// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::service::{make_service_fn, service_fn};
use hyper::{http, Body, Method, Request, Response, StatusCode};
use serde_json::json;
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

        let make_svc = make_service_fn(move |_| {
            let invocation_processor = invocation_processor.clone();

            let service = service_fn(move |req| Self::handler(req, invocation_processor.clone()));

            async move { Ok::<_, Infallible>(service) }
        });

        let port = u16::try_from(AGENT_PORT).expect("AGENT_PORT is too large");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let server_builder = hyper::Server::try_bind(&addr)?;

        let server = server_builder.serve(make_svc);

        // start hyper http server
        if let Err(e) = server.await {
            error!("Failed to start the Lifecycle Listener {e}");
            return Err(e.into());
        }

        Ok(())
    }

    async fn handler(
        req: Request<Body>,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> http::Result<Response<Body>> {
        match (req.method(), req.uri().path()) {
            (&Method::POST, START_INVOCATION_PATH) => {
                Self::start_invocation_handler(req, invocation_processor).await
            }
            (&Method::POST, END_INVOCATION_PATH) => {
                match Self::end_invocation_handler(req, invocation_processor).await {
                    Ok(response) => Ok(response),
                    Err(e) => {
                        error!("Failed to end invocation {e}");
                        Ok(Response::builder()
                            .status(500)
                            .body(Body::empty())
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

    pub async fn start_invocation_handler(
        req: Request<Body>,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> http::Result<Response<Body>> {
        debug!("Received start invocation request");
        let (parts, body) = req.into_parts();
        match hyper::body::to_bytes(body).await {
            Ok(b) => {
                let body = b.to_vec();
                let mut processor = invocation_processor.lock().await;

                let headers = Self::headers_to_map(parts.headers);

                let (span_id, trace_id, parent_id) = processor.on_invocation_start(headers, body);

                let mut response = Response::builder().status(200);

                // If a `SpanContext` exists, then tell the tracer to use it.
                // todo: update this whole code with DatadogHeaderPropagator::inject
                // since this logic looks messy
                if let Some(sp) = &processor.extracted_span_context {
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
                }

                drop(processor);

                response.body(Body::from(
                    json!({
                        "span_id": span_id,
                        "trace_id": trace_id,
                        "parent_id": parent_id
                    })
                    .to_string(),
                ))
            }
            Err(e) => {
                error!("Could not read start invocation request body {e}");

                Response::builder()
                    .status(400)
                    .body(Body::from("Could not read start invocation request body"))
            }
        }
    }

    async fn end_invocation_handler(
        req: Request<Body>,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> http::Result<Response<Body>> {
        debug!("Received end invocation request");
        let (parts, body) = req.into_parts();
        let parsed_body = serde_json::from_slice::<serde_json::Value>(
            &hyper::body::to_bytes(body).await.unwrap_or_default(),
        );
        let mut parsed_status: Option<String> = None;
        if let Some(status_code) = parsed_body.unwrap_or_default().get("statusCode") {
            parsed_status = Some(status_code.to_string());
        }
        let headers = parts.headers;

        // todo: fix this, code is a copy of the existing logic in Go, not accounting
        // when a 128 bit trace id exist
        let mut trace_id = 0;
        if let Some(header) = headers.get("x-datadog-trace-id") {
            if let Ok(header_value) = header.to_str() {
                trace_id = header_value.parse::<u64>().unwrap_or(0);
            }
        }

        let mut span_id = 0;
        if let Some(header) = headers.get("x-datadog-span-id") {
            if let Ok(header_value) = header.to_str() {
                span_id = header_value.parse::<u64>().unwrap_or(0);
            }
        }

        let mut parent_id = 0;
        if let Some(header) = headers.get("x-datadog-parent-id") {
            if let Ok(header_value) = header.to_str() {
                parent_id = header_value.parse::<u64>().unwrap_or(0);
            }
        }

        invocation_processor.lock().await.on_invocation_end(
            trace_id,
            span_id,
            parent_id,
            parsed_status,
        );

        Response::builder()
            .status(200)
            .body(Body::from(json!({}).to_string()))
    }

    fn hello_handler() -> http::Result<Response<Body>> {
        warn!("[DEPRECATED] Please upgrade your tracing library, the /hello route is deprecated");
        Response::builder()
            .status(200)
            .body(Body::from(json!({}).to_string()))
    }

    fn headers_to_map(headers: http::HeaderMap) -> HashMap<String, String> {
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
