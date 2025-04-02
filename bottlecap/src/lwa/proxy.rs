use crate::lifecycle::invocation::generate_span_id;
use crate::{lifecycle::invocation::processor::Processor, lifecycle::listener::Listener};
use hyper::body::Bytes;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{
    body::HttpBody,
    client::HttpConnector,
    http,
    http::request::Parts,
    service::{make_service_fn, service_fn},
    Body, Client, Error, HeaderMap, Request, Response, Server, Uri,
};
use hyper_proxy::{Intercept, Proxy, ProxyConnector};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::{net::SocketAddr, sync::Arc};
use tokio::{sync::Mutex, task::JoinHandle};
use tracing::{debug, error};

#[must_use]
pub fn start_lwa_proxy(invocation_processor: Arc<Mutex<Processor>>) -> Option<JoinHandle<()>> {
    if let Some((proxy_socket, aws_runtime_uri)) = parse_env_addresses() {
        debug!(
            "LWA: proxy enabled with proxy URI: {} and AWS runtime: {}",
            proxy_socket, aws_runtime_uri
        );
        let proxied_client = match build_proxy(aws_runtime_uri.clone()) {
            Some(client) => client,
            None => return None,
        };

        let proxy_task_handle = tokio::spawn({
            async move {
                let proxy_server = Server::bind(&proxy_socket).serve(make_service_fn(move |_| {
                    let processor = Arc::clone(&invocation_processor);
                    let uri = aws_runtime_uri.clone();
                    let client = Arc::clone(&proxied_client);
                    async move {
                        Ok::<_, Error>(service_fn(move |req| {
                            intercept_payload(
                                req,
                                Arc::clone(&client),
                                Arc::clone(&processor),
                                uri.clone(),
                            )
                        }))
                    }
                }));

                if let Err(e) = proxy_server.await {
                    error!("LWA: proxy server error: {}", e);
                }
            }
        });
        Some(proxy_task_handle)
    } else {
        None
    }
}

fn build_proxy(uri_to_intercept: Uri) -> Option<Arc<Client<ProxyConnector<HttpConnector>>>> {
    match ProxyConnector::from_proxy(
        HttpConnector::new(),
        Proxy::new(Intercept::All, uri_to_intercept),
    ) {
        Ok(proxy_connector) => Some(Arc::new(
            Client::builder().build::<_, Body>(proxy_connector),
        )),
        Err(e) => {
            error!("LWA: Error creating proxy connector: {}", e);
            None
        }
    }
}

fn parse_env_addresses() -> Option<(SocketAddr, Uri)> {
    let aws_lwa_proxy_lambda_runtime_api = match std::env::var("AWS_LWA_PROXY_LAMBDA_RUNTIME_API") {
        Ok(uri) => match uri.parse::<Uri>() {
            Ok(parsed_uri) => {
                let host = parsed_uri.host();
                let port = parsed_uri.port_u16();
                if let (Some(host), Some(port)) = (host, port) {
                    if host == "localhost" {
                        error!("LWA: Cannot use localhost as host in AWS_LWA_PROXY_LAMBDA_RUNTIME_API, use 127.0.0.1 instead");
                        return None;
                    }
                    format!("{host}:{port}")
                        .parse::<SocketAddr>()
                        .map_err(|e| {
                            error!(
                                "LWA: cannot parse socket address from host and port {}: {}",
                                uri, e
                            );
                            ();
                        })
                        .ok()
                } else {
                    error!("LWA: Missing host or port in parsed URI {}", parsed_uri);
                    None
                }
            }
            Err(e) => {
                error!(
                    "LWA: Error parsing uri from AWS_LWA_PROXY_LAMBDA_RUNTIME_API: {}",
                    e
                );
                None
            }
        },
        Err(_) => None,
    };
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
    match (aws_lwa_proxy_lambda_runtime_api, aws_runtime_api) {
        (Some(proxy_uri), Some(aws_runtime_addr)) => Some((proxy_uri, aws_runtime_addr)),
        _ => None,
    }
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
    intercepted: Request<Body>,
    client: Arc<Client<ProxyConnector<HttpConnector>>>,
    processor: Arc<Mutex<Processor>>,
    aws_runtime_addr: Uri,
) -> Result<Response<Body>, Error> {
    // request received from lambda handler directed to AWS runtime API
    // it can be either invocation/next, or a lambda handler response to it
    let (intercepted_parts, intercepted_body) = intercepted.into_parts();
    let waited_intercepted_body = intercepted_body.collect().await?.to_bytes();

    error!("LWA: Intercepted request: {:?}", intercepted_parts);
    // error!(
    //     "LWA: Intercepted request body: {:?}",
    //     waited_intercepted_body
    // );

    let forward_intercepted = forward_request(
        aws_runtime_addr,
        &intercepted_parts,
        waited_intercepted_body.clone().into(),
    )
    .await?;

    // response after forwarding to AWS runtime API
    let response_to_intercepted_req = client.request(forward_intercepted).await?;

    let (resp_part, resp_body) = response_to_intercepted_req.into_parts();
    let resp_payload = resp_body.collect().await?.to_bytes();

    error!("LWA: Intercepted resp: {:?}", &resp_part);
    // error!("LWA: Intercepted resp body: {:?}", resp_payload);

    let mut response_to_intercepted_req = Response::builder()
        .status(resp_part.status)
        .version(resp_part.version)
        .body(Body::from(resp_payload.clone()))
        .unwrap();
    *response_to_intercepted_req.headers_mut() = resp_part.headers.clone();

    match (intercepted_parts.method, intercepted_parts.uri.path()) {
        (hyper::Method::GET, "/2018-06-01/runtime/invocation/next") => {
            on_get_next_response(&processor, resp_part, &resp_payload).await
        }
        (hyper::Method::POST, path)
            if path.starts_with("/2018-06-01/runtime/invocation/")
                && path.ends_with("/response") =>
        {
            on_post_invocation(&processor, &waited_intercepted_body).await;
            // only parsing of the original request (handler -> runtime API) is needed so
            // the original response can be used
            Ok(response_to_intercepted_req)
        }
        _ => Ok(response_to_intercepted_req),
    }
}

async fn on_get_next_response(
    processor: &Arc<Mutex<Processor>>,
    resp_part: http::response::Parts,
    resp_payload: &Bytes,
) -> Result<Response<Body>, Error> {
    // intercepted invocation/next. The *response body* contains the payload of
    // the request that the lambda handler will see

    let inner_payload = serde_json::from_slice::<Value>(resp_payload).unwrap_or_else(|_| json!({}));

    // error!("LWA: payload wrapped in body {}", inner_payload);
    // Response is not cloneable, so it must be built again
    let body = serde_json::to_vec(&inner_payload).unwrap();
    let mut rebuild_response = Response::builder()
        .status(resp_part.status)
        .version(resp_part.version)
        .body(Body::from(body.clone()))
        .unwrap();
    *rebuild_response.headers_mut() = resp_part.headers;
    let (parent_id, _) = Listener::universal_instrumentation_start(
        rebuild_response.headers(),
        body.into(),
        Arc::clone(processor),
    )
    .await;

    {
        let mut invocation_processor = processor.lock().await;
        invocation_processor.set_reparenting(generate_span_id(), parent_id);
    }
    // complete forwarding to the lambda handler
    Ok(rebuild_response)
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
        let header_name = HeaderName::from_bytes(k.as_bytes()).unwrap();
        headers.insert(header_name, HeaderValue::from_str(&v).unwrap());
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

async fn forward_request(
    aws_runtime_addr: Uri,
    req_parts: &Parts,
    req_body: Body,
) -> Result<Request<Body>, Error> {
    let mut redirect_uri = aws_runtime_addr.into_parts();
    redirect_uri.path_and_query = Some(req_parts.uri.path_and_query().unwrap().clone());
    let new_uri = Uri::from_parts(redirect_uri).unwrap();

    let request_body_waited = req_body.collect().await?.to_bytes();

    let redirected_request = Request::builder()
        .method(req_parts.method.clone())
        .uri(new_uri)
        .body(Body::from(request_body_waited.clone()))
        .unwrap_or_else(|e| {
            error!("LWA: Error building redirected request: {}", e);
            Request::new(Body::empty())
        });
    Ok(redirected_request)
}

#[cfg(test)]
mod tests {
    use crate::config::{AwsConfig, Config};
    use crate::lifecycle::invocation::processor::Processor;
    use crate::lwa::proxy::start_lwa_proxy;
    use crate::tags::provider::Provider;
    use crate::traces::propagation::error::Error;
    use dogstatsd::metric::EMPTY_TAGS;
    use hyper::service::{make_service_fn, service_fn};

    use crate::LAMBDA_RUNTIME_SLUG;

    use dogstatsd::aggregator::Aggregator;
    use hyper::{Body, Client, Response, Uri};
    use std::sync::{Arc, Mutex};
    use std::{
        collections::HashMap,
        env,
        net::SocketAddr,
        time::{Duration, Instant},
    };
    use tokio::sync::Mutex as TokioMutex;
    use tokio::time::sleep;

    #[tokio::test]
    async fn noop_proxy() {
        let proxy_uri = "127.0.0.1:12345";
        let final_uri = "127.0.0.1:12344";

        env::set_var("AWS_LWA_PROXY_LAMBDA_RUNTIME_API", proxy_uri);
        env::set_var("AWS_LAMBDA_RUNTIME_API", final_uri);

        let final_destination = tokio::spawn(async {
            hyper::Server::bind(&SocketAddr::from(([127, 0, 0, 1], 12344)))
                .serve(make_service_fn(|_| async {
                    Ok::<_, Error>(service_fn(|_| async {
                        Ok::<_, Error>(Response::new(hyper::Body::from(
                            "Response from AWS LAMBDA RUNTIME API",
                        )))
                    }))
                }))
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

        let client = Client::builder().build_http::<Body>();
        let uri_with_schema = format!("http://{proxy_uri}");
        let mut ask_proxy = client
            .get(Uri::try_from(uri_with_schema.clone()).unwrap())
            .await;

        while ask_proxy.is_err() {
            println!(
                "Retrying request to proxy, err: {}",
                ask_proxy.err().unwrap()
            );
            sleep(Duration::from_millis(50)).await;
            ask_proxy = client
                .get(Uri::try_from(uri_with_schema.clone()).unwrap())
                .await;
        }

        let ask_proxy = ask_proxy.unwrap();

        let body_bytes = hyper::body::to_bytes(ask_proxy.into_body()).await.unwrap();
        let bytes = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(bytes, "Response from AWS LAMBDA RUNTIME API");

        proxy_task_handle.abort();
        final_destination.abort();

        env::remove_var("AWS_LWA_PROXY_LAMBDA_RUNTIME_API");
        env::remove_var("AWS_LAMBDA_RUNTIME_API");
    }
}
