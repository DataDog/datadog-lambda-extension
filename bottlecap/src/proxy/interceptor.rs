use crate::{
    appsec,
    config::{aws::AwsConfig, Config},
    lifecycle::invocation::processor::Processor as InvocationProcessor,
    lwa, EXTENSION_HOST,
};
use axum::{
    body::{Body, Bytes},
    extract::{FromRequest, Path, Request, State},
    http::{self, Request as HttpRequest, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use http_body_util::BodyExt;
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::Mutex, task::JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

/// The Lambda Function requires the `/opt/datadog_wrapper` to be executed.
/// Since it sets port to `9000`.
///
/// This port is used to intercept requests coming from the AWS Lambda Runtime Interface Client (RIC).
const INTERCEPTOR_DEFAULT_PORT: u16 = 9000;

type InterceptorState = (
    Arc<Config>,
    AwsConfig,
    Arc<Client<HttpConnector, Body>>,
    Arc<Mutex<InvocationProcessor>>,
    Arc<Mutex<JoinSet<()>>>,
);

pub fn start(
    config: Arc<Config>,
    aws_config: AwsConfig,
    invocation_processor: Arc<Mutex<InvocationProcessor>>,
) -> Result<CancellationToken, Box<dyn std::error::Error>> {
    let socket = get_proxy_socket_address(&aws_config.aws_lwa_proxy_lambda_runtime_api);
    let shutdown_token = CancellationToken::new();

    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(std::time::Duration::from_secs(5)));

    let client = Client::builder(TokioExecutor::new())
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(8)
        .build(connector);

    let tasks = Arc::new(Mutex::new(JoinSet::new()));
    let state: InterceptorState = (
        config,
        aws_config,
        Arc::new(client),
        invocation_processor,
        Arc::clone(&tasks),
    );

    let shutdown_token_clone = shutdown_token.clone();
    tokio::spawn(async move {
        let server = TcpListener::bind(&socket)
            .await
            .expect("Failed to bind socket");
        let router = make_router(state);
        debug!("PROXY | Starting API runtime proxy on {socket}");
        axum::serve(server, router)
            .with_graceful_shutdown(graceful_shutdown(tasks, shutdown_token_clone))
            .await
            .expect("Failed to start API runtime proxy");
    });

    Ok(shutdown_token)
}

fn make_router(state: InterceptorState) -> Router {
    Router::new()
        .route("/", get(passthrough_proxy))
        .route(
            "/{api_version}/runtime/invocation/next",
            get(invocation_next_proxy),
        )
        .route(
            "/{api_version}/runtime/invocation/{request_id}/response",
            post(invocation_response_proxy),
        )
        .route(
            "/{api_version}/runtime/invocation/{request_id}/error",
            post(passthrough_proxy),
        )
        .fallback(passthrough_proxy)
        .with_state(state)
}

async fn graceful_shutdown(tasks: Arc<Mutex<JoinSet<()>>>, shutdown_token: CancellationToken) {
    shutdown_token.cancelled().await;
    debug!("PROXY | Shutdown signal received, shutting down");

    let mut tasks = tasks.lock().await;
    while let Some(task) = tasks.join_next().await {
        if let Some(e) = task.err() {
            error!("PROXY | Shutdown error: {e}");
        }
    }
}

/// Given an optional String representing the LWA proxy lambda runtime API,
/// return a `SocketAddr` that can be used to bind the proxy server.
///
/// If the LWA proxy lambda runtime API is not provided, the default Extension
/// host and port will be used.
///
fn get_proxy_socket_address(aws_lwa_proxy_lambda_runtime_api: &Option<String>) -> SocketAddr {
    if let Some(socket_addr) = aws_lwa_proxy_lambda_runtime_api
        .as_ref()
        .and_then(|uri_str| lwa::get_lwa_proxy_socket_address(uri_str).ok())
    {
        debug!("PROXY | get_proxy_socket_address | LWA proxy detected");
        return socket_addr;
    }

    let uri = format!("{EXTENSION_HOST}:{INTERCEPTOR_DEFAULT_PORT}");
    uri.parse::<SocketAddr>()
        .expect("Failed to parse socket address")
}

async fn invocation_next_proxy(
    Path(api_version): Path<String>,
    State((config, aws_config, client, invocation_processor, tasks)): State<InterceptorState>,
    request: Request,
) -> Response {
    debug!("PROXY | invocation_next_proxy | api_version: {api_version}");
    let (parts, body_bytes) = match extract_request_body(request).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to extract request body: {e}"),
            )
                .into_response();
        }
    };

    let (intercepted_parts, intercepted_bytes) =
        match proxy_request(&client, &aws_config, parts, body_bytes).await {
            Ok(r) => r,
            Err(e) => {
                error!("PROXY | passthrough_proxy | error proxying request: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to build forward response: {e}"),
                )
                    .into_response();
            }
        };

    // LWA
    if aws_config.aws_lwa_proxy_lambda_runtime_api.is_some() {
        let mut tasks = tasks.lock().await;

        let invocation_processor = invocation_processor.clone();
        let intercepted_parts = intercepted_parts.clone();
        let intercepted_bytes = intercepted_bytes.clone();
        tasks.spawn(async move {
            lwa::process_invocation_next(
                &invocation_processor,
                &intercepted_parts,
                &intercepted_bytes,
            )
            .await;
        });
    }

    // K9 / ASM
    if appsec::is_enabled(&config) {
        // TODO: do something here
    }

    match build_forward_response(intercepted_parts, intercepted_bytes) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | passthrough_proxy | error building forward response: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build forward response: {e}"),
            )
                .into_response()
        }
    }
}

async fn invocation_response_proxy(
    Path((api_version, request_id)): Path<(String, String)>,
    State((config, aws_config, client, invocation_processor, tasks)): State<InterceptorState>,
    request: Request,
) -> Response {
    debug!(
        "PROXY | invocation_response_proxy | api_version: {api_version}, request_id: {request_id}"
    );
    let (parts, body_bytes) = match extract_request_body(request).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to extract request body: {e}"),
            )
                .into_response();
        }
    };

    // LWA
    if aws_config.aws_lwa_proxy_lambda_runtime_api.is_some() {
        let mut tasks = tasks.lock().await;

        let invocation_processor = invocation_processor.clone();
        let body_bytes = body_bytes.clone();
        tasks.spawn(async move {
            lwa::process_invocation_response(&invocation_processor, &body_bytes).await;
        });
    }

    // K9 / ASM
    if appsec::is_enabled(&config) {
        // TODO: do something here
    }

    let (intercepted_parts, intercepted_bytes) =
        match proxy_request(&client, &aws_config, parts, body_bytes).await {
            Ok(r) => r,
            Err(e) => {
                error!("PROXY | passthrough_proxy | error proxying request: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to proxy request: {e}"),
                )
                    .into_response();
            }
        };

    match build_forward_response(intercepted_parts, intercepted_bytes) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | passthrough_proxy | error building forward response: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build forward response: {e}"),
            )
                .into_response()
        }
    }
}

async fn passthrough_proxy(
    State((_, aws_config, client, _, _)): State<InterceptorState>,
    request: Request,
) -> Response {
    let (parts, body_bytes) = match extract_request_body(request).await {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | passthrough_proxy | error extracting request body: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to extract request body: {e}"),
            )
                .into_response();
        }
    };

    let (intercepted_parts, intercepted_bytes) =
        match proxy_request(&client, &aws_config, parts, body_bytes).await {
            Ok(r) => r,
            Err(e) => {
                error!("PROXY | passthrough_proxy | error proxying request: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to proxy request: {e}"),
                )
                    .into_response();
            }
        };

    match build_forward_response(intercepted_parts, intercepted_bytes) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | passthrough_proxy | error building forward response: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build forward response: {e}"),
            )
                .into_response()
        }
    }
}

async fn proxy_request(
    client: &Client<HttpConnector, Body>,
    aws_config: &AwsConfig,
    parts: http::request::Parts,
    body_bytes: Bytes,
) -> Result<(http::response::Parts, Bytes), Box<dyn std::error::Error>> {
    let request = build_proxy_request(aws_config, parts, body_bytes)?;
    debug!("PROXY | proxy_request | calling {}", request.uri());
    let intercepted_response = client.request(request).await?;
    let (parts, body) = intercepted_response.into_parts();
    let bytes = body.collect().await?.to_bytes();

    Ok((parts, bytes))
}

fn build_forward_response(
    parts: http::response::Parts,
    body_bytes: Bytes,
) -> Result<Response<Body>, Box<dyn std::error::Error>> {
    let mut forward_response = Response::builder()
        .status(parts.status)
        .version(parts.version);

    if let Some(h) = forward_response.headers_mut() {
        *h = parts.headers;
    }

    let forward_response = forward_response.body(Body::from(body_bytes))?;

    Ok(forward_response)
}

async fn extract_request_body(
    request: Request,
) -> Result<(hyper::http::request::Parts, Bytes), Box<dyn std::error::Error>> {
    let (parts, body) = request.into_parts();
    let bytes = Bytes::from_request(Request::from_parts(parts.clone(), body), &()).await?;

    Ok((parts, bytes))
}

fn build_proxy_request(
    aws_config: &AwsConfig,
    parts: http::request::Parts,
    body_bytes: Bytes,
) -> Result<Request<Body>, Box<dyn std::error::Error>> {
    let uri = parts.uri.clone();

    let target_path = uri
        .path_and_query()
        .map(std::string::ToString::to_string)
        .unwrap_or(uri.path().to_string());

    let target_uri = format!("http://{}{}", aws_config.runtime_api, target_path);
    let uri = target_uri.parse::<Uri>()?;

    let mut request = HttpRequest::builder()
        .method(&parts.method)
        .uri(uri)
        .version(parts.version);

    if let Some(h) = request.headers_mut() {
        *h = parts.headers.clone();
    }

    let request = request.body(Body::from(body_bytes))?;

    Ok(request)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::Mutex,
        time::{Duration, Instant},
    };
    use tokio::sync::Mutex as TokioMutex;

    use dogstatsd::{aggregator::Aggregator as MetricsAggregator, metric::EMPTY_TAGS};
    use http_body_util::Full;
    use hyper::{server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;

    use crate::{config::Config, tags::provider::Provider, LAMBDA_RUNTIME_SLUG};

    use super::*;

    #[tokio::test]
    async fn test_noop_proxy() {
        let aws_lwa_lambda_runtime_api = "127.0.0.1:12345";
        let aws_lambda_runtime_api = "127.0.0.1:12344";

        let final_destination = tokio::spawn(async move {
            let listener = TcpListener::bind(aws_lambda_runtime_api)
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
        let metrics_aggregator = Arc::new(Mutex::new(
            MetricsAggregator::new(EMPTY_TAGS, 1024).unwrap(),
        ));

        let aws_config = AwsConfig {
            region: "us-east-1".to_string(),
            aws_access_key_id: "AKIDEXAMPLE".to_string(),
            aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
            function_name: "arn:some-function".to_string(),
            sandbox_init_time: Instant::now(),
            aws_container_credentials_full_uri: String::new(),
            aws_container_authorization_token: String::new(),
            runtime_api: aws_lambda_runtime_api.to_string(),
            aws_lwa_proxy_lambda_runtime_api: Some(aws_lwa_lambda_runtime_api.to_string()),
            exec_wrapper: None,
        };
        let invocation_processor = Arc::new(TokioMutex::new(InvocationProcessor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            &aws_config,
            metrics_aggregator,
        )));

        let proxy_handle = start(config.clone(), aws_config, invocation_processor)
            .expect("Failed to start API runtime proxy");
        let https = HttpConnector::new();
        let client = Client::builder(hyper_util::rt::TokioExecutor::new())
            .build::<_, http_body_util::Full<prost::bytes::Bytes>>(https);

        let uri_with_schema = format!("http://{aws_lwa_lambda_runtime_api}");
        let mut ask_proxy = client
            .get(Uri::try_from(uri_with_schema.clone()).unwrap())
            .await;

        while ask_proxy.is_err() {
            error!(
                "Retrying request to proxy, err: {}",
                ask_proxy.err().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
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
        let _ = proxy_handle.cancel();
        final_destination.abort();
    }
}
