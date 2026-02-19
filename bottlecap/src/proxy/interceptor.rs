use crate::lifecycle::invocation::processor_service::InvocationProcessorHandle;
use crate::lifecycle::invocation::triggers::IdentifiedTrigger;
use crate::traces::propagation::DatadogCompositePropagator;
use crate::{
    appsec::processor::Processor as AppSecProcessor, config::aws::AwsConfig,
    extension::EXTENSION_HOST, lwa, proxy::tee_body::TeeBodyWithCompletion,
};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{self, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
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
    Arc<AwsConfig>,
    Arc<Client<HttpConnector, Body>>,
    InvocationProcessorHandle,
    Option<Arc<Mutex<AppSecProcessor>>>,
    Arc<DatadogCompositePropagator>,
    Arc<Mutex<JoinSet<()>>>,
);

pub fn start(
    aws_config: Arc<AwsConfig>,
    invocation_processor_handle: InvocationProcessorHandle,
    appsec_processor: Option<Arc<Mutex<AppSecProcessor>>>,
    propagator: Arc<DatadogCompositePropagator>,
) -> Result<CancellationToken, Box<dyn std::error::Error>> {
    let socket = get_proxy_socket_address(aws_config.aws_lwa_proxy_lambda_runtime_api.as_ref());
    let shutdown_token = CancellationToken::new();

    let mut connector = HttpConnector::new();
    connector.set_connect_timeout(Some(std::time::Duration::from_secs(5)));

    let client = Client::builder(TokioExecutor::new())
        .pool_idle_timeout(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(8)
        .build(connector);

    let tasks = Arc::new(Mutex::new(JoinSet::new()));
    let state: InterceptorState = (
        aws_config,
        Arc::new(client),
        invocation_processor_handle,
        appsec_processor,
        propagator,
        tasks.clone(),
    );

    let tasks_clone = tasks.clone();
    let shutdown_token_clone = shutdown_token.clone();
    tokio::spawn(async move {
        let server = TcpListener::bind(&socket)
            .await
            .expect("Failed to bind socket");
        let router = make_router(state);
        debug!("PROXY | Starting API runtime proxy on {socket}");
        axum::serve(server, router)
            .with_graceful_shutdown(graceful_shutdown(tasks_clone, shutdown_token_clone))
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
            post(invocation_error_proxy),
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
fn get_proxy_socket_address(aws_lwa_proxy_lambda_runtime_api: Option<&String>) -> SocketAddr {
    if let Some(socket_addr) = aws_lwa_proxy_lambda_runtime_api
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
    State((aws_config, client, invocation_processor, appsec_processor, propagator, tasks)): State<
        InterceptorState,
    >,
    request: Request,
) -> Response {
    debug!("PROXY | invocation_next_proxy | api_version: {api_version}");
    let (parts, body) = request.into_parts();
    let request = match build_proxy_request(&aws_config, parts, body) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | invocation_next_proxy | error building proxy request");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build proxy request: {e}"),
            )
                .into_response();
        }
    };

    debug!("PROXY | invocation_next_proxy | proxying {}", request.uri());
    let intercepted_response = match client.request(request).await {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | invocation_next_proxy | error proxying request");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to proxy: {e}"),
            )
                .into_response();
        }
    };

    let (intercepted_parts, intercepted_body) = intercepted_response.into_parts();

    // Intercepted body is what the AWS Lambda event will be set as.
    // Which is what we want to process.
    let (intercepted_tee_body, intercepted_completion_receiver) =
        TeeBodyWithCompletion::new(intercepted_body);

    let mut join_set = tasks.lock().await;
    let intercepted_parts_clone = intercepted_parts.clone();
    let propagator = Arc::clone(&propagator);
    join_set.spawn(async move {
        if let Ok(body) = intercepted_completion_receiver.await {
            debug!("PROXY | invocation_next_proxy | intercepted body completed");

            if let Some(appsec_processor) = appsec_processor
                && let Some(request_id) = intercepted_parts_clone
                    .headers
                    .get("Lambda-Runtime-Aws-Request-Id")
                    .and_then(|v| v.to_str().ok())
            {
                {
                    if let Ok(trigger) = IdentifiedTrigger::from_slice(&body) {
                        appsec_processor
                            .lock()
                            .await
                            .process_invocation_next(request_id, &trigger)
                            .await;
                    }
                }
            }

            if aws_config.aws_lwa_proxy_lambda_runtime_api.is_some() {
                lwa::process_invocation_next(
                    &invocation_processor,
                    &intercepted_parts_clone,
                    &body,
                    Arc::clone(&propagator),
                )
                .await;
            }
        }
    });

    match build_forward_response(intercepted_parts, Body::new(intercepted_tee_body)) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | invocation_next_proxy | error building response: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build response: {e}"),
            )
                .into_response()
        }
    }
}

async fn invocation_response_proxy(
    Path((api_version, request_id)): Path<(String, String)>,
    State((aws_config, client, invocation_processor, appsec_processor, _, tasks)): State<
        InterceptorState,
    >,
    request: Request,
) -> Response {
    debug!(
        "PROXY | invocation_response_proxy | api_version: {api_version}, request_id: {request_id}"
    );
    let (parts, body) = request.into_parts();
    let (outgoing_tee_body, outgoing_completion_receiver) = TeeBodyWithCompletion::new(body);

    // The outgoing body is what the final user will see.
    // Which is what AWS Lambda returns, in turn what we want to process.
    let mut join_set = tasks.lock().await;
    let aws_config_clone = aws_config.clone();
    join_set.spawn(async move {
        if let Ok(body) = outgoing_completion_receiver.await {
            debug!("PROXY | invocation_response_proxy | intercepted outgoing body completed");
            if let Some(appsec_processor) = appsec_processor {
                appsec_processor
                    .lock()
                    .await
                    .process_invocation_result(&request_id, &body)
                    .await;
            }

            if aws_config_clone.aws_lwa_proxy_lambda_runtime_api.is_some() {
                lwa::process_invocation_response(&invocation_processor, &body).await;
            }
        }
    });

    let request = match build_proxy_request(&aws_config, parts, outgoing_tee_body) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | invocation_response_proxy | error building proxy request");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build proxy request: {e}"),
            )
                .into_response();
        }
    };

    debug!(
        "PROXY | invocation_response_proxy | proxying {}",
        request.uri()
    );
    // Send the streaming request
    let intercepted_response = match client.request(request).await {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | invocation_response_proxy | error proxying request");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to proxy: {e}"),
            )
                .into_response();
        }
    };

    let (intercepted_parts, intercepted_body) = intercepted_response.into_parts();
    match build_forward_response(intercepted_parts, Body::new(intercepted_body)) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | invocation_response_proxy | error building response: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build response: {e}"),
            )
                .into_response()
        }
    }
}

async fn invocation_error_proxy(
    Path((api_version, request_id)): Path<(String, String)>,
    state: State<InterceptorState>,
    request: Request,
) -> Response {
    debug!("PROXY | invocation_error_proxy | api_version: {api_version}, request_id: {request_id}");
    let State((_, _, _, appsec_processor, _, _)) = &state;
    if let Some(appsec_processor) = appsec_processor {
        // Marking any outstanding security context as finalized by sending a blank response.
        appsec_processor
            .lock()
            .await
            .process_invocation_result(&request_id, &Bytes::from("{}"))
            .await;
    }

    passthrough_proxy(state, request).await
}

async fn passthrough_proxy(
    State((aws_config, client, _, _, _, _)): State<InterceptorState>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();

    let request = match build_proxy_request(&aws_config, parts, body) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | passthrough_proxy | error building proxy request");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build proxy request: {e}"),
            )
                .into_response();
        }
    };

    // Send the streaming request
    let intercepted_response = match client.request(request).await {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | passthrough_proxy | error proxying request");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to proxy: {e}"),
            )
                .into_response();
        }
    };

    let (intercepted_parts, intercepted_body) = intercepted_response.into_parts();
    match build_forward_response(intercepted_parts, Body::new(intercepted_body)) {
        Ok(r) => r,
        Err(e) => {
            error!("PROXY | passthrough_proxy | error building response: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build response: {e}"),
            )
                .into_response()
        }
    }
}

fn clean_proxy_headers(headers: &mut HeaderMap) {
    // Remove hop-by-hop headers that shouldn't be forwarded
    headers.remove("connection");
    headers.remove("upgrade");
    headers.remove("proxy-connection");
    headers.remove("proxy-authenticate");
    headers.remove("proxy-authorization");
    headers.remove("te");

    // For streaming, we preserve transfer-encoding and content-length
    // The underlying HTTP implementation will handle them correctly
}

fn build_forward_response<B>(
    parts: http::response::Parts,
    body: B,
) -> Result<Response<B>, Box<dyn std::error::Error>>
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let mut response_builder = Response::builder()
        .status(parts.status)
        .version(parts.version);

    if let Some(headers) = response_builder.headers_mut() {
        *headers = parts.headers;
        clean_proxy_headers(headers);
    }

    let response = response_builder.body(body)?;

    Ok(response)
}

fn build_proxy_request<B>(
    aws_config: &AwsConfig,
    parts: http::request::Parts,
    body: B,
) -> Result<Request<Body>, Box<dyn std::error::Error>>
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let uri = parts.uri.clone();
    let target_path = uri
        .path_and_query()
        .map(std::string::ToString::to_string)
        .unwrap_or(uri.path().to_string());
    let target_uri = format!("http://{}{}", aws_config.runtime_api, target_path);
    let parsed_uri = target_uri.parse::<Uri>()?;

    let mut request_builder = hyper::Request::builder()
        .method(&parts.method)
        .uri(parsed_uri)
        .version(parts.version);

    if let Some(headers) = request_builder.headers_mut() {
        *headers = parts.headers.clone();
        clean_proxy_headers(headers);
    }

    let hyper_body = Body::new(body);
    let request = request_builder.body(hyper_body)?;

    Ok(request)
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use std::{collections::HashMap, time::Duration};
    use tokio::{sync::Mutex as TokioMutex, time::Instant};

    use dogstatsd::{aggregator_service::AggregatorService, metric::EMPTY_TAGS};
    use http_body_util::Full;
    use hyper::{server::conn::http1, service::service_fn};
    use hyper_util::rt::TokioIo;

    use super::*;
    use crate::lifecycle::invocation::processor_service::InvocationProcessorService;
    use crate::{
        LAMBDA_RUNTIME_SLUG, appsec::processor::Error::FeatureDisabled as AppSecFeatureDisabled,
        config::Config, tags::provider::Provider, traces::propagation::DatadogCompositePropagator,
    };

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
                .expect("failed to serve HTTP connection");
        });

        let config = Arc::new(Config::default());
        let tags_provider = Arc::new(Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));
        let (service, handle) =
            AggregatorService::new(EMPTY_TAGS, 1024).expect("failed to create aggregator service");

        tokio::spawn(service.run());

        let metrics_aggregator = handle;
        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".to_string(),
            function_name: "arn:some-function".to_string(),
            sandbox_init_time: Instant::now(),
            runtime_api: aws_lambda_runtime_api.to_string(),
            aws_lwa_proxy_lambda_runtime_api: Some(aws_lwa_lambda_runtime_api.to_string()),
            exec_wrapper: None,
            initialization_type: "on-demand".into(),
        });
        let propagator = Arc::new(DatadogCompositePropagator::new(Arc::clone(&config)));
        let (invocation_processor_handle, invocation_processor_service) =
            InvocationProcessorService::new(
                Arc::clone(&tags_provider),
                Arc::clone(&config),
                Arc::clone(&aws_config),
                metrics_aggregator,
                Arc::clone(&propagator),
            );
        tokio::spawn(async move {
            invocation_processor_service.run().await;
        });

        let appsec_processor = match AppSecProcessor::new(&config) {
            Ok(p) => Some(Arc::new(TokioMutex::new(p))),
            Err(AppSecFeatureDisabled) => None,
            Err(e) => {
                error!(
                    "PROXY | aap | error creating App & API Protection processor, the feature will be disabled: {e}"
                );
                None
            }
        };

        let proxy_handle = start(
            aws_config,
            invocation_processor_handle,
            appsec_processor,
            propagator,
        )
        .expect("Failed to start API runtime proxy");
        let https = HttpConnector::new();
        let client = Client::builder(hyper_util::rt::TokioExecutor::new())
            .build::<_, http_body_util::Full<prost::bytes::Bytes>>(https);

        let uri_with_schema = format!("http://{aws_lwa_lambda_runtime_api}");
        let mut ask_proxy = client
            .get(Uri::try_from(uri_with_schema.clone()).expect("failed to parse URI"))
            .await;

        while let Err(err) = ask_proxy {
            error!("Retrying request to proxy, err: {err}");
            tokio::time::sleep(Duration::from_millis(50)).await;
            ask_proxy = client
                .get(Uri::try_from(uri_with_schema.clone()).expect("failed to parse URI"))
                .await;
        }

        let (_, body) = ask_proxy.expect("failed to retrieve response").into_parts();
        let body_bytes = body
            .collect()
            .await
            .expect("failed to collect response body")
            .to_bytes();

        let bytes =
            String::from_utf8(body_bytes.to_vec()).expect("failed to convert bytes to String");
        assert_eq!(bytes, "Response from AWS LAMBDA RUNTIME API");
        // Send shutdown signal to the proxy server
        proxy_handle.cancel();
        final_destination.abort();
    }
}
