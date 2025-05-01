use crate::{
    config::{aws::AwsConfig, Config},
    lifecycle::invocation::processor::Processor as InvocationProcessor,
    EXTENSION_HOST,
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

type InterceptorState = (AwsConfig, Arc<Client<HttpConnector, Body>>);

impl Interceptor {
    pub fn new(
        config: Arc<Config>,
        aws_config: &AwsConfig,
        invocation_processor: Arc<Mutex<InvocationProcessor>>,
    ) -> Self {
        Self {
            config,
            aws_config: aws_config.clone(),
            invocation_processor,
        }
    }

    pub async fn start(&self) -> Result<oneshot::Sender<()>, Box<dyn std::error::Error>> {
        let socket = Self::get_proxy_socket_address(&self.aws_config);
        let server = TcpListener::bind(&socket).await?;
        let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();

        let router = self.make_router();
        debug!("Starting API runtime proxy on {socket}");
        axum::serve(server, router).await?;

        Ok(shutdown_tx)
    }

    pub fn make_router(&self) -> Router {
        let connector = HttpConnector::new();
        let client = Arc::new(Client::builder(TokioExecutor::new()).build(connector));

        let state = (self.aws_config.clone(), client);

        Router::new()
            .route("/", get(Self::passthrough_proxy))
            .route(
                "/{api_version}/runtime/invocation/next",
                get(Self::passthrough_proxy),
            )
            .route(
                "/{api_version}/runtime/invocation/{request_id}/response",
                post(Self::invocation_response_proxy),
            )
            .route(
                "/{api_version}/runtime/invocation/{request_id}/error",
                post(Self::passthrough_proxy),
            )
            .fallback(Self::passthrough_proxy)
            .with_state(state)
    }

    async fn invocation_response_proxy(
        Path((api_version, request_id)): Path<(String, String)>,
        State((aws_config, client)): State<InterceptorState>,
        request: Request,
    ) -> Response {
        debug!(
            "PROXY | invocation_response_proxy | api_version: {api_version}, request_id: {request_id}"
        );
        let (parts, body_bytes) = match Self::extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to extract request body: {e}"),
                )
                    .into_response();
            }
        };

        match Self::proxy_request(&client, &aws_config, parts, body_bytes).await {
            Ok(r) => r,
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to proxy request: {e}"),
            )
                .into_response(),
        }
    }

    async fn passthrough_proxy(
        State((aws_config, client)): State<InterceptorState>,
        request: Request,
    ) -> Response {
        let (parts, body_bytes) = match Self::extract_request_body(request).await {
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

        match Self::proxy_request(&client, &aws_config, parts, body_bytes).await {
            Ok(r) => r,
            Err(e) => {
                error!("PROXY | passthrough_proxy | error proxying request: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to proxy request: {e}"),
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
    ) -> Result<Response, Box<dyn std::error::Error>> {
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
        debug!("PROXY | proxy_request | calling {target_path}");
        let response = client.request(request).await?;

        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        let mut forward_response = Response::builder().status(parts.status);

        if let Some(h) = forward_response.headers_mut() {
            *h = parts.headers;
        }

        let forward_response = forward_response.body(Body::from(bytes))?;

        Ok(forward_response)
    }

    async fn extract_request_body(
        request: Request,
    ) -> Result<(hyper::http::request::Parts, Bytes), Box<dyn std::error::Error>> {
        let (parts, body) = request.into_parts();
        let bytes = Bytes::from_request(Request::from_parts(parts.clone(), body), &()).await?;

        Ok((parts, bytes))
    }

    fn get_proxy_socket_address(_aws_config: &AwsConfig) -> SocketAddr {
        let uri = format!("{EXTENSION_HOST}:{INTERCEPTOR_DEFAULT_PORT}");
        uri.parse::<SocketAddr>()
            .expect("Failed to parse socket address")
    }
}
