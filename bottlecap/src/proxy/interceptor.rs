use crate::{
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
use tokio::{
    net::TcpListener,
    sync::{oneshot, Mutex},
};
use tracing::{debug, error};

/// The Lambda Function requires the `/opt/datadog_wrapper` to be executed.
///
/// This port is used to intercept requests coming from the AWS Lambda Runtime Interface Client (RIC).
const INTERCEPTOR_DEFAULT_PORT: u16 = 9000;

pub struct Interceptor {
    pub config: Arc<Config>,
    pub aws_config: AwsConfig,
    /// Processors
    pub invocation_processor: Arc<Mutex<InvocationProcessor>>,
}

type InterceptorState = (
    Arc<Config>,
    AwsConfig,
    Arc<Client<HttpConnector, Body>>,
    Arc<Mutex<InvocationProcessor>>,
);

pub async fn start(
    config: Arc<Config>,
    aws_config: AwsConfig,
    invocation_processor: Arc<Mutex<InvocationProcessor>>,
) -> Result<oneshot::Sender<()>, Box<dyn std::error::Error>> {
    let socket = get_proxy_socket_address(&aws_config.aws_lwa_proxy_lambda_runtime_api);
    let server = TcpListener::bind(&socket).await?;
    let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();

    let router = make_router(config, aws_config, invocation_processor);
    debug!("PROXY | Starting API runtime proxy on {socket}");
    axum::serve(server, router).await?;

    Ok(shutdown_tx)
}

fn make_router(
    config: Arc<Config>,
    aws_config: AwsConfig,
    invocation_processor: Arc<Mutex<InvocationProcessor>>,
) -> Router {
    let connector = HttpConnector::new();
    // TODO(duncanista): find a good number of idle timeout and max idle per host.
    let client = Client::builder(TokioExecutor::new()).build(connector);

    let state: InterceptorState = (
        config.clone(),
        aws_config.clone(),
        Arc::new(client),
        invocation_processor.clone(),
    );

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
    State((_, aws_config, client, invocation_processor)): State<InterceptorState>,
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
                    format!("Failed to proxy request: {e}"),
                )
                    .into_response();
            }
        };

    if aws_config.aws_lwa_proxy_lambda_runtime_api.is_some() {
        lwa::process_invocation_next(
            &invocation_processor,
            &intercepted_parts,
            &intercepted_bytes,
        );
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
    State((_, aws_config, client, invocation_processor)): State<InterceptorState>,
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

    if aws_config.aws_lwa_proxy_lambda_runtime_api.is_some() {
        lwa::process_invocation_response(&invocation_processor, &body_bytes);
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
    State((_, aws_config, client, _)): State<InterceptorState>,
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
