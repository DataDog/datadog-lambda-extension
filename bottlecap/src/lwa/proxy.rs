use crate::lifecycle::invocation::processor::Processor;
use crate::traces::trace_processor::{ServerlessTraceProcessor, TraceProcessor};
use hyper::body::Bytes;
use hyper::client::HttpConnector;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Error, Request, Response, Server, Uri};
use hyper_proxy::{Intercept, Proxy, ProxyConnector};
use rand::random;
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error};

#[must_use]
pub fn start_lwa_proxy(
    invocation_processor: Arc<Mutex<Processor>>,
    trace_processor: Arc<Mutex<ServerlessTraceProcessor>>,
) -> Option<JoinHandle<()>> {
    if let Some((proxy_socket, aws_runtime_uri)) = parse_env_addresses() {
        debug!(
            "LWA proxy enabled with proxy URI: {} and AWS runtime: {}",
            proxy_socket, aws_runtime_uri
        );
        let proxied_client = match build_proxy(aws_runtime_uri.clone()) {
            Some(client) => client,
            None => return None,
        };

        let proxy_task_handle = tokio::spawn({
            async move {
                let proxy_server = Server::bind(&proxy_socket).serve(make_service_fn(move |_| {
                    let trace_processor = Arc::clone(&trace_processor);
                    let processor = Arc::clone(&invocation_processor);
                    let uri = aws_runtime_uri.clone();
                    let client = Arc::clone(&proxied_client);
                    async move {
                        Ok::<_, Error>(service_fn(move |req| {
                            intercept_tracer_headers(
                                req,
                                Arc::clone(&client),
                                Arc::clone(&trace_processor),
                                Arc::clone(&processor),
                                uri.clone(),
                            )
                        }))
                    }
                }));

                if let Err(e) = proxy_server.await {
                    error!("LWA proxy server error: {}", e);
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
            error!("Error creating proxy connector: {}", e);
            None
        }
    }
}

fn parse_env_addresses() -> Option<(SocketAddr, Uri)> {
    let aws_lwa_proxy_lambda_runtime_api = match std::env::var("AWS_LWA_PROXY_LAMBDA_RUNTIME_API") {
        Ok(uri) => match uri.parse::<Uri>() {
            Ok(parsed_uri) => {
                let host = parsed_uri.host()?;
                let port = parsed_uri.port_u16()?;
                let socket_addr = format!("{host}:{port}").parse::<SocketAddr>().ok()?;
                Some(socket_addr)
            }
            Err(e) => {
                error!("Error parsing AWS_LWA_PROXY_LAMBDA_RUNTIME_API: {}", e);
                None
            }
        },
        Err(_) => None,
    };

    let aws_runtime_api = match std::env::var("AWS_LAMBDA_RUNTIME_API") {
        Ok(env_uri) => match format!("http://{env_uri}").parse() {
            Ok(parsed_uri) => Some(parsed_uri),
            Err(e) => {
                error!("Error parsing AWS_LAMBDA_RUNTIME_API: {}", e);
                None
            }
        },
        Err(e) => {
            error!("Error retrieving AWS_LAMBDA_RUNTIME_API: {}", e);
            None
        }
    };
    match (aws_lwa_proxy_lambda_runtime_api, aws_runtime_api) {
        (Some(proxy_uri), Some(aws_runtime_addr)) => Some((proxy_uri, aws_runtime_addr)),
        _ => None,
    }
}

async fn intercept_tracer_headers(
    req: Request<Body>,
    client: Arc<Client<ProxyConnector<HttpConnector>>>,
    span_generator: Arc<Mutex<ServerlessTraceProcessor>>,
    processor: Arc<Mutex<Processor>>,
    aws_runtime_addr: Uri,
) -> Result<Response<Body>, Error> {
    let (req_parts, req_body) = req.into_parts();

    let mut redirect_uri = aws_runtime_addr.into_parts();
    redirect_uri.path_and_query = Some(req_parts.uri.path_and_query().unwrap().clone());
    let new_uri = Uri::from_parts(redirect_uri).unwrap();

    let redirected_request = Request::builder()
        .method(req_parts.method.clone())
        .uri(new_uri)
        .body(req_body)
        .unwrap_or_else(|e| {
            error!("Error building redirected request: {}", e);
            Request::new(Body::empty())
        });

    let response = client.request(redirected_request).await?;

    match (req_parts.method, req_parts.uri.path()) {
        (hyper::Method::GET, "/2018-06-01/runtime/invocation/next") => {
            let (resp_part, resp_body) = response.into_parts();
            let resp_payload =
                process_tracing_headers(span_generator, processor, resp_body).await?;

            let mut rebuild_response = Response::builder()
                .status(resp_part.status)
                .version(resp_part.version)
                .body(Body::from(resp_payload))
                .unwrap();

            *rebuild_response.headers_mut() = resp_part.headers;
            Ok(rebuild_response)
        }
        (hyper::Method::GET, path)
            if path.starts_with("/2018-06-01/runtime/invocation/")
                && path.ends_with("/response") =>
        {
            processor
                .lock()
                .await
                .on_invocation_end(random(), 0, 0, None);
            Ok(response)
        }
        _ => Ok(response),
    }
}

async fn process_tracing_headers(
    span_generator: Arc<Mutex<impl TraceProcessor>>,
    processor: Arc<Mutex<Processor>>,
    resp_body: Body,
) -> Result<Bytes, Error> {
    let resp_payload = hyper::body::to_bytes(resp_body).await?;
    let resp_json = deserialize_json(Ok(resp_payload.clone())).unwrap();
    let vec = serde_json::to_vec(&resp_json);
    if vec.is_ok() {
        let headers = resp_json.get("headers").unwrap();
        let h = headers
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect();

        let (span_id, trace_id, parent_id) = processor.lock().await.on_invocation_start(h, vec.unwrap());
        span_generator
            .lock()
            .await
            .override_ids(trace_id, parent_id, span_id);
    }
    Ok(resp_payload)
}

fn deserialize_json(response: Result<Bytes, Error>) -> Option<Value> {
    match response {
        Ok(bytes) => serde_json::from_slice(bytes.as_ref()).unwrap_or_else(|e| {
            error!("Error deserializing response body: {}", e);
            None
        }),
        Err(e) => {
            error!("Error reading response body: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AwsConfig, Config};
    use crate::tags::provider::Provider;
    use crate::LAMBDA_RUNTIME_SLUG;
    use datadog_trace_obfuscation::obfuscation_config;
    use hyper::Uri;
    use std::collections::HashMap;
    use std::env;
    use std::net::SocketAddr;
    use std::time::Duration;
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

        let invocation_processor = Arc::new(TokioMutex::new(Processor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            &AwsConfig {
                region: "us-east-1".to_string(),
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
                function_name: "arn:some-function".to_string(),
            },
        )));

        let trace_processor = Arc::new(TokioMutex::new(ServerlessTraceProcessor {
            obfuscation_config: Arc::new(obfuscation_config::ObfuscationConfig::new().unwrap()),
            resolved_api_key: "api_key".to_string(),
            override_trace_id: None,
            root_parent_id: None,
            aws_lambda_span_id: None,
        }));

        let proxy_task_handle =
            start_lwa_proxy(invocation_processor, trace_processor).expect("Failed to start proxy");

        let client = Client::builder().build_http::<Body>();
        let uri_with_schema = format!("http://{}", proxy_uri);
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
