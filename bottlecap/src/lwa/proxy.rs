use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{http, Request, Response, Uri};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioIo;
use lifecycle::invocation::{generate_span_id, processor::Processor};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::{sync::Mutex, task::JoinHandle};
use tracing::{debug, error};

use crate::lifecycle;
use crate::lifecycle::listener::Listener;

/// The sender can be used to trigger a graceful shutdown of the proxy server.
pub type ShutdownSender = mpsc::Sender<()>;

#[must_use]
#[allow(clippy::module_name_repetitions)]
pub fn start_lwa_proxy(invocation_processor: Arc<Mutex<Processor>>) -> Option<ShutdownSender> {
    let (proxy_socket, aws_runtime_uri) = parse_env_addresses()?;
    debug!(
        "LWA: proxy enabled with proxy URI: {} and AWS runtime: {}",
        proxy_socket, aws_runtime_uri
    );

    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    tokio::spawn({
        async move {
            let proxy_server = TcpListener::bind(&proxy_socket)
                .await
                .expect("LWA: Failed to bind LWA proxy socket");

            loop {
                tokio::select! {
                    accept_result = proxy_server.accept() => {
                        match accept_result {
                            Ok((tcp_stream, _)) => {
                                let io = TokioIo::new(tcp_stream);
                                let async_invocation_processor_cp = Arc::clone(&invocation_processor);
                                let async_aws_runtime_uri_cp = aws_runtime_uri.clone();

                                tokio::task::spawn(async move {
                                    if let Err(err) = http1::Builder::new()
                                        .preserve_header_case(true)
                                        .title_case_headers(true)
                                        .serve_connection(
                                            io,
                                            service_fn(move |req| {
                                                intercept_payload(
                                                    req,
                                                    Arc::clone(&async_invocation_processor_cp),
                                                    async_aws_runtime_uri_cp.clone(),
                                                )
                                            }),
                                        )
                                        // .with_upgrades()
                                        .await
                                    {
                                        error!("LWA: Failed to serve connection: {err:?}");
                                    }
                                });
                            }
                            Err(e) => {
                                error!("LWA: Failed to accept LWA connection: {e}");
                                break;
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("LWA: Received shutdown signal, terminating proxy server loop.");
                        break;
                    }
                }
            }
        }
    });
    Some(shutdown_tx)
}

fn parse_env_addresses() -> Option<(SocketAddr, Uri)> {
    let aws_lwa_proxy_lambda_runtime_api = std::env::var("AWS_LWA_LAMBDA_RUNTIME_API_PROXY")
    .ok()
    .and_then(|uri| match uri.parse::<Uri>() {
            Ok(parsed_uri) => {
                let host = parsed_uri.host();
                let port = parsed_uri.port_u16();
                if let (Some(host), Some(port)) = (host, port) {
                    if host == "localhost" {
                        error!("LWA: Cannot use localhost as host in AWS_LWA_LAMBDA_RUNTIME_API_PROXY, use 127.0.0.1 instead");
                        return None;
                    }
                    format!("{host}:{port}")
                        .parse::<SocketAddr>()
                        .map_err(|e| {
                            error!(
                                "LWA: cannot parse socket address from host and port {}: {}",
                                uri, e
                            );
                        })
                        .ok()
                } else {
                    error!("LWA: Missing host or port in parsed URI {}", parsed_uri);
                    None
                }
            }
            Err(e) => {
                error!(
                    "LWA: Error parsing uri from AWS_LWA_LAMBDA_RUNTIME_API_PROXY: {}",
                    e
                );
                None
            }
        });
    let aws_runtime_api = match std::env::var("AWS_LAMBDA_RUNTIME_API") {
        Ok(env_uri) => match format!("http://{env_uri}").parse() {
            Ok(parsed_uri) => Some(parsed_uri),
            Err(e) => {
                error!("LWA: Error parsing AWS_LAMBDA_RUNTIME_API: {}", e);
                None
            }
        },
        Err(e) => {
            error!("LWA: Error retrieving AWS_LAMBDA_RUNTIME_API: {}", e);
            None
        }
    };
    aws_lwa_proxy_lambda_runtime_api.zip(aws_runtime_api)
}

// Example of flow
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

async fn intercept_payload(
    intercepted: Request<hyper::body::Incoming>,
    processor: Arc<Mutex<Processor>>,
    aws_runtime_addr: Uri,
) -> Result<Response<BoxBody<Bytes, Infallible>>, hyper::Error> {
    // request received from lambda handler directed to AWS runtime API
    // it can be either invocation/next, or a lambda handler response to it
    let (intercepted_parts, intercepted_body) = intercepted.into_parts();
    let waited_intercepted_body = BodyExt::collect(intercepted_body).await?.to_bytes();

    let path = intercepted_parts.uri.path();
    let method = intercepted_parts.method.clone();
    let mut uni_instr_start = None;
    if hyper::Method::POST == method
        && path.starts_with("/2018-06-01/runtime/invocation/")
        && path.ends_with("/response")
    {
        uni_instr_start = Some(tokio::spawn({
            let processor = Arc::clone(&processor);
            let waited_intercepted_body = waited_intercepted_body.clone();
            async move {
                on_post_invocation(&processor, &waited_intercepted_body).await;
            }
        }));
    }

    let forward_intercepted = build_forward_request(
        aws_runtime_addr,
        &intercepted_parts,
        waited_intercepted_body.clone(),
    );

    // Create HTTP client
    let https = HttpConnector::new();
    let client = Client::builder(hyper_util::rt::TokioExecutor::new())
        .build::<_, http_body_util::Full<prost::bytes::Bytes>>(https);

    // response after forwarding to AWS runtime API
    let response_to_intercepted = match client.request(forward_intercepted).await {
        Ok(response) => response,
        Err(e) => {
            error!("LWA: Error forwarding request to AWS runtime: {}", e);
            return Ok(Response::new(BoxBody::new(Full::new(Bytes::new()))));
        }
    };

    let (resp_part, resp_body) = response_to_intercepted.into_parts();
    let resp_payload = BodyExt::collect(resp_body).await?.to_bytes();

    let mut uni_instr_end = None;
    if hyper::Method::GET == method && path == "/2018-06-01/runtime/invocation/next" {
        let status_async = resp_part.status;
        let version_async = resp_part.version;
        let headers_async = resp_part.headers.clone();
        let resp_payload_async = resp_payload.clone();

        uni_instr_end = Some(tokio::spawn({
            async move {
                on_get_next_response(
                    &processor,
                    status_async,
                    version_async,
                    &headers_async,
                    &resp_payload_async,
                )
                .await;
            }
        }));
    }

    let mut forward_response = Response::builder()
        .status(resp_part.status)
        .version(resp_part.version)
        .body(BoxBody::new(Full::new(resp_payload)))
        .unwrap_or_else(|e| {
            error!("LWA: Error building forwarded response: {}", e);
            Response::new(BoxBody::new(Full::new(Bytes::new())))
        });

    *forward_response.headers_mut() = resp_part.headers.clone();

    wait_for_uni_instr(uni_instr_start, "start").await;
    wait_for_uni_instr(uni_instr_end, "end").await;

    Ok(forward_response)
}

async fn wait_for_uni_instr(uni_instr_task: Option<JoinHandle<()>>, instr_type: &str) {
    if let Some(handle) = uni_instr_task {
        let before_universal_instrumentation = Instant::now();
        if let Err(e) = handle.await {
            error!(
                "LWA: Error waiting for async universal instrumentation {} task: {}",
                instr_type, e
            );
        }
        debug!(
            "LWA: Time taken for universal instrumentation {} : {}ms",
            instr_type,
            before_universal_instrumentation.elapsed().as_millis()
        );
    }
}

async fn on_get_next_response(
    processor: &Arc<Mutex<Processor>>,
    status: http::StatusCode,
    version: http::Version,
    headers: &HeaderMap,
    resp_payload: &Bytes,
) {
    // intercepted invocation/next. The *response body* contains the payload of
    // the request that the lambda handler will see

    let inner_payload = serde_json::from_slice::<Value>(resp_payload).unwrap_or_else(|_| json!({}));

    // Response is not cloneable, so it must be built again
    let body = serde_json::to_vec(&inner_payload).unwrap_or_else(|e| {
        error!("LWA: Error serializing GET response body: {}", e);
        vec![]
    });
    let mut rebuild_response = Response::builder()
        .status(status)
        .version(version)
        .body(Full::new(Bytes::from(body.clone())))
        .unwrap_or_else(|e| {
            error!(
                "LWA: Error building universal instrumentation start request: {}",
                e
            );
            Response::new(Full::new(Bytes::new()))
        });
    *rebuild_response.headers_mut() = headers.clone();

    let (parent_id, _) = Listener::universal_instrumentation_start(
        rebuild_response.headers(),
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

async fn on_post_invocation(processor: &Arc<Mutex<Processor>>, waited_intercepted_body: &Bytes) {
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

    let _ =
        Listener::universal_instrumentation_end(&headers, body_bytes.into(), Arc::clone(processor))
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

fn build_forward_request(
    aws_runtime_addr: Uri,
    req_parts: &http::request::Parts,
    req_body: Bytes,
) -> hyper::Request<http_body_util::Full<prost::bytes::Bytes>> {
    let mut redirect_uri = aws_runtime_addr.clone().into_parts();
    redirect_uri.path_and_query = req_parts.uri.path_and_query().cloned();

    let new_uri = Uri::from_parts(redirect_uri).unwrap_or_else(|e| {
        error!("LWA: Error building new URI{}", e);
        aws_runtime_addr
    });

    Request::builder()
        .method(req_parts.method.clone())
        .uri(new_uri)
        .body(Full::new(req_body))
        .unwrap_or_else(|e| {
            error!("LWA: Error building redirected request: {}", e);
            Request::new(Full::new(Bytes::new()))
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::config::{AwsConfig, Config};
    use crate::lifecycle::invocation::processor::Processor;
    use crate::lwa::proxy::start_lwa_proxy;
    use crate::tags::provider::Provider;
    use http_body_util::BodyExt;

    use bytes::Bytes;
    use dogstatsd::metric::EMPTY_TAGS;
    use http_body_util::Full;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::client::legacy::connect::HttpConnector;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpListener;

    use crate::LAMBDA_RUNTIME_SLUG;

    use dogstatsd::aggregator::Aggregator;
    use hyper::{Response, Uri};
    use std::sync::{Arc, Mutex};
    use std::{
        collections::HashMap,
        env,
        time::{Duration, Instant},
    };
    use tokio::sync::Mutex as TokioMutex;
    use tokio::time::sleep;

    #[tokio::test]
    async fn noop_proxy() {
        let proxy_uri = "127.0.0.1:12345";
        let final_uri = "127.0.0.1:12344";

        env::set_var("AWS_LWA_LAMBDA_RUNTIME_API_PROXY", proxy_uri);
        env::set_var("AWS_LAMBDA_RUNTIME_API", final_uri);

        let final_destination = tokio::spawn(async move {
            let listener = TcpListener::bind(final_uri)
                .await
                .expect("Failed to bind final destination socket");
            let (tcp_stream, _) = listener
                .accept()
                .await
                .expect("LWA: Failed to accept LWA connection");
            let io = TokioIo::new(tcp_stream);
            http1::Builder::new()
                .preserve_header_case(true)
                .title_case_headers(true)
                .serve_connection(
                    io,
                    service_fn(move |_req| async move {
                        Ok::<_, std::convert::Infallible>(Response::new(Full::new(Bytes::from(
                            "Response from AWS LAMBDA RUNTIME API",
                        ))))
                    }),
                )
                .await
                .unwrap();
        });
        let config = Arc::new(Config::default());
        let tags_provider = Arc::new(Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));
        let metrics_aggregator = Arc::new(Mutex::new(Aggregator::new(EMPTY_TAGS, 1024).unwrap()));

        let invocation_processor = Arc::new(TokioMutex::new(Processor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            &AwsConfig {
                region: "us-east-1".to_string(),
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
                function_name: "arn:some-function".to_string(),
                sandbox_init_time: Instant::now(),
                aws_container_credentials_full_uri: String::new(),
                aws_container_authorization_token: String::new(),
            },
            metrics_aggregator,
        )));

        let proxy_task_handle =
            start_lwa_proxy(invocation_processor).expect("Failed to start proxy");

        let https = HttpConnector::new();
        let client = Client::builder(hyper_util::rt::TokioExecutor::new())
            .build::<_, http_body_util::Full<prost::bytes::Bytes>>(https);

        let uri_with_schema = format!("http://{proxy_uri}");
        let mut ask_proxy = client
            .get(Uri::try_from(uri_with_schema.clone()).unwrap())
            .await;

        while ask_proxy.is_err() {
            error!(
                "Retrying request to proxy, err: {}",
                ask_proxy.err().unwrap()
            );
            sleep(Duration::from_millis(50)).await;
            ask_proxy = client
                .get(Uri::try_from(uri_with_schema.clone()).unwrap())
                .await;
        }

        let body_bytes = ask_proxy
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();

        let bytes = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(bytes, "Response from AWS LAMBDA RUNTIME API");

        // Send shutdown signal to the proxy server
        let _ = proxy_task_handle.send(()).await;
        final_destination.abort();

        env::remove_var("AWS_LWA_LAMBDA_RUNTIME_API_PROXY");
        env::remove_var("AWS_LAMBDA_RUNTIME_API");
    }
}
