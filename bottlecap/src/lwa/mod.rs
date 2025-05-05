use axum::http::{self, HeaderName, HeaderValue};
use bytes::Bytes;
use hyper::{HeaderMap, Uri};
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::sync::Mutex;
use tracing::{debug, error};

use crate::lifecycle::{
    invocation::{generate_span_id, processor::Processor as InvocationProcessor},
    listener::Listener as LifecycleListener,
};

pub fn get_lwa_proxy_socket_address(
    uri_str: &str,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let uri = uri_str.parse::<Uri>()?;

    let host = uri.host().ok_or("LWA: Missing host in URI")?;
    let port = uri.port_u16().ok_or("LWA: Missing port in URI")?;

    if host == "localhost" {
        error!("LWA | Cannot use localhost as host in AWS_LWA_LAMBDA_RUNTIME_API_PROXY, use 127.0.0.1 instead");
        return Err("Cannot use localhost as host, use 127.0.0.1 instead".into());
    }

    let socket_addr = format!("{host}:{port}")
        .parse::<SocketAddr>()
        .map_err(|e| {
            error!(
                "LWA | Cannot parse socket address from host and port {}: {}",
                uri, e
            );
            e
        })?;

    Ok(socket_addr)
}

pub fn process_invocation_next(
    processor: &Arc<Mutex<InvocationProcessor>>,
    intercepted_parts: &http::response::Parts,
    intercepted_body: &Bytes,
) {
    let processor = Arc::clone(processor);
    let intercepted_parts = intercepted_parts.clone();
    let intercepted_body = intercepted_body.clone();

    tokio::spawn(async move {
        on_get_invocation_next(&processor, &intercepted_parts, &intercepted_body).await;
    });
}

async fn on_get_invocation_next(
    processor: &Arc<Mutex<InvocationProcessor>>,
    parts: &http::response::Parts,
    body_bytes: &Bytes,
) {
    let headers = parts.headers.clone();
    // intercepted invocation/next. The *response body* contains the payload of
    // the request that the lambda handler will see

    let inner_payload = serde_json::from_slice::<Value>(body_bytes).unwrap_or_else(|e| {
        error!("LWA: Error parsing response payload as JSON: {}", e);
        if body_bytes.len() < 1024 {
            debug!(
                "LWA: Invalid JSON payload: {:?}",
                String::from_utf8_lossy(body_bytes)
            );
        }
        json!({})
    });

    // Response is not cloneable, so it must be built again
    let body = serde_json::to_vec(&inner_payload).unwrap_or_else(|e| {
        error!("LWA: Error serializing GET response body: {}", e);
        vec![]
    });

    let (parent_id, _) = LifecycleListener::universal_instrumentation_start(
        &parts.headers,
        body.into(),
        Arc::clone(processor),
    )
    .await;

    let request_id = headers
        .get("lambda-runtime-aws-request-id")
        .unwrap_or(&HeaderValue::from_static(""))
        .to_str()
        .unwrap_or_default()
        .to_string();

    {
        let mut invocation_processor = processor.lock().await;
        invocation_processor.add_reparenting(request_id, generate_span_id(), parent_id);
    }
}

pub fn process_invocation_response(
    processor: &Arc<Mutex<InvocationProcessor>>,
    intercepted_body: &Bytes,
) {
    let processor = Arc::clone(processor);
    let intercepted_body = intercepted_body.clone();
    tokio::spawn(async move {
        on_post_invocation_response(&processor, &intercepted_body).await;
    });
}

async fn on_post_invocation_response(
    processor: &Arc<Mutex<InvocationProcessor>>,
    waited_intercepted_body: &Bytes,
) {
    let inner_payload =
        serde_json::from_slice::<Value>(waited_intercepted_body).unwrap_or_else(|_| json!({}));

    let body_bytes = inner_payload
        .get("body")
        .map(|body| serde_json::to_vec(body).unwrap_or_else(|_| vec![]))
        .unwrap_or_default();

    let header_map = inner_header(&inner_payload);
    let mut headers = HeaderMap::new();
    for (k, v) in header_map {
        let header_name = HeaderName::from_bytes(k.as_bytes()).unwrap_or_else(|e| {
            error!("LWA: Error creating header name: {}", e);
            HeaderName::from_static("x-unknown-header")
        });
        headers.insert(
            header_name,
            HeaderValue::from_str(&v).unwrap_or_else(|e| {
                error!("LWA: Error creating header value: {}", e);
                HeaderValue::from_static("")
            }),
        );
    }

    let _ = LifecycleListener::universal_instrumentation_end(
        &headers,
        body_bytes.into(),
        Arc::clone(processor),
    )
    .await;
}

fn inner_header(inner_payload: &Value) -> HashMap<String, String> {
    let headers = if let Some(body) = inner_payload.get("headers") {
        serde_json::from_value::<HashMap<String, String>>(body.clone())
            .unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    };
    headers
}
