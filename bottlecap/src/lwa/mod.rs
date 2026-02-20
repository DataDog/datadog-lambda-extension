/// LWA (Lambda Web Adapter)
///
/// Example of flow
//   Starting up proxy
//     LWA: proxy enabled with proxy URI: 127.0.0.1:9002 and AWS runtime: http://127.0.0.1:9001/
//   Extension is ready, blocking on GET /invocation/next. Intercepting it in theis LWA proxy
//     LWA: Intercepted request: Parts { method: GET, uri: /2018-06-01/runtime/invocation/next, version: HTTP/1.1, headers: {"user-agent": "aws-lambda-rust/aws-lambda-adapter/0.9.0", "host": "127.0.0.1:9002"} }
//     LWA: Intercepted request body: b""
//  AWS lambda service returns from GET /invocation/next with the incoming invocation. LWA wraps it into the body. LWA proxy intercepts it
//     LWA: Intercepted resp: Parts { status: 200, version: HTTP/1.1, headers: {"content-type": "application/json", "lambda-runtime-aws-request-id": "8442603f-da10-42d2-bf58-c33a31978aad", "lambda-runtime-deadline-ms": "1741965555094", "lambda-runtime-invoked-function-arn": "arn:aws:lambda:us-east-1:425362996713:function:ag-lwa-stack-lambda", "lambda-runtime-trace-id": "Root=1-67d448e8-3ae320be0e2cdf2f53ccdaba;Lineage=1:73f724a8:0", "date": "Fri, 14 Mar 2025 15:19:06 GMT", "content-length": "927"} }
//     LWA: Intercepted resp body: b"{\"version\":\"2.0\",\"routeKey\":\"$default\",\"rawPath\":\"/\",\"rawQueryString\":\"\",\"headers\":{\"x-amzn-tls-cipher-suite\":\"TLS_AES_128_GCM_SHA256\",\"x-amzn-tls-version\":\"TLSv1.3\",\"x-amzn-trace-id\":\"Root=1-67d448e8-3ae320be0e2cdf2f53ccdaba\",\"x-forwarded-proto\":\"https\",\"host\":\"e366vgzulqwityxor4e6nfkdam0axzew.lambda-url.us-east-1.on.aws\",\"x-forwarded-port\":\"443\",\"x-forwarded-for\":\"70.107.97.101\",\"accept\":\"*/*\",\"user-agent\":\"curl/7.81.0\"},\"requestContext\":{\"accountId\":\"anonymous\",\"apiId\":\"e366vgzulqwityxor4e6nfkdam0axzew\",\"domainName\":\"e366vgzulqwityxor4e6nfkdam0axzew.lambda-url.us-east-1.on.aws\",\"domainPrefix\":\"e366vgzulqwityxor4e6nfkdam0axzew\",\"http\":{\"method\":\"GET\",\"path\":\"/\",\"protocol\":\"HTTP/1.1\",\"sourceIp\":\"70.107.97.101\",\"userAgent\":\"curl/7.81.0\"},\"requestId\":\"8442603f-da10-42d2-bf58-c33a31978aad\",\"routeKey\":\"$default\",\"stage\":\"$default\",\"time\":\"14/Mar/2025:15:19:04 +0000\",\"timeEpoch\":1741965544908},\"isBase64Encoded\":false}"
//  Lambda Runtime processes the request and when it's done, LWA invokes POST to runtime/invocation/REQ_ID/response. The body has the response. LWA proxy intercepts it
//     LWA: Intercepted request: Parts { method: POST, uri: /2018-06-01/runtime/invocation/8442603f-da10-42d2-bf58-c33a31978aad/response, version: HTTP/1.1, headers: {"user-agent": "aws-lambda-rust/aws-lambda-adapter/0.9.0", "host": "127.0.0.1:9002", "content-length": "238"} }
//     LWA: Intercepted request body: b"{\"statusCode\":200,\"headers\":{\"date\":\"Fri, 14 Mar 2025 15:19:06 GMT\",\"content-length\":\"34\",\"content-type\":\"text/html; charset=utf-8\"},\"multiValueHeaders\":{},\"body\":\"<h1>Hello, Website with span!<h1>\\n\",\"isBase64Encoded\":false,\"cookies\":[]}
//  AWS Lambda service responds to the POST with a 202. LWA proxy intercepts it
//     LWA: Intercepted resp: Parts { status: 202, version: HTTP/1.1, headers: {"content-type": "application/json", "date": "Fri, 14 Mar 2025 15:19:06 GMT", "content-length": "16"} }
//     LWA: Intercepted resp body: b"{\"status\":\"OK\"}\n"
//  Extension is again ready, blocking on GET /invocation/next
//     LWA: Intercepted request: Parts { method: GET, uri: /2018-06-01/runtime/invocation/next, version: HTTP/1.1, headers: {"user-agent": "aws-lambda-rust/aws-lambda-adapter/0.9.0", "host": "127.0.0.1:9002"} }
//     LWA: Intercepted request body: b""
use axum::http::{self, HeaderName, HeaderValue};
use bytes::Bytes;
use hyper::{HeaderMap, Uri};
use serde_json::{Value, json};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tracing::{debug, error};

use crate::{
    http::headers_to_map,
    lifecycle::{
        invocation::{generate_span_id, processor_service::InvocationProcessorHandle},
        listener::Listener as LifecycleListener,
    },
    traces::propagation::DatadogCompositePropagator,
};

pub fn get_lwa_proxy_socket_address(
    uri_str: &str,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let uri = uri_str.parse::<Uri>()?;

    let host = uri.host().ok_or("LWA: Missing host in URI")?;
    let port = uri.port_u16().ok_or("LWA: Missing port in URI")?;

    if host == "localhost" {
        error!(
            "LWA | Cannot use localhost as host in AWS_LWA_LAMBDA_RUNTIME_API_PROXY, use 127.0.0.1 instead"
        );
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

pub async fn process_invocation_next(
    invocation_processor_handle: &InvocationProcessorHandle,
    parts: &http::response::Parts,
    body_bytes: &Bytes,
    propagator: Arc<DatadogCompositePropagator>,
) {
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

    let headers = headers_to_map(&parts.headers);
    let payload_value =
        serde_json::from_slice::<Value>(&body.clone()).unwrap_or_else(|_| json!({}));
    let extracted_span_context = InvocationProcessorHandle::extract_span_context(
        &headers,
        &payload_value,
        Arc::clone(&propagator),
    );
    let request_id = headers
        .get("lambda-runtime-aws-request-id")
        .map(std::string::ToString::to_string);

    LifecycleListener::universal_instrumentation_start(
        headers,
        payload_value,
        invocation_processor_handle.clone(),
    )
    .await;

    let mut parent_id = 0;
    if let Some(sp) = extracted_span_context {
        parent_id = sp.span_id;
    }

    if let Some(request_id) = request_id {
        let _ = invocation_processor_handle
            .add_reparenting(request_id.clone(), generate_span_id(), parent_id)
            .await;
    }
}

pub async fn process_invocation_response(
    invocation_processor_handle: &InvocationProcessorHandle,
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

    LifecycleListener::universal_instrumentation_end(
        &headers,
        body_bytes.into(),
        invocation_processor_handle.clone(),
    )
    .await;
}

fn inner_header(inner_payload: &Value) -> HashMap<String, String> {
    if let Some(body) = inner_payload.get("headers") {
        serde_json::from_value::<HashMap<String, String>>(body.clone())
            .unwrap_or_else(|_| HashMap::new())
    } else {
        HashMap::new()
    }
}
