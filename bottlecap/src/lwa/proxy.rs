use crate::lifecycle::invocation::processor::Processor;
use crate::traces::trace_processor::{ServerlessTraceProcessor, TraceProcessor};
use hyper::body::Bytes;
use hyper::client::HttpConnector;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Error, Request, Response, Server, Uri};
use hyper_proxy::{Intercept, Proxy, ProxyConnector};
use rand::random;
use serde_json::Value;
use std::collections::HashMap;
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
    // request received from lambda handler directed to AWS runtime API
    // it can be either invocation/next, or a lambda handler response to it
    let (req_parts, req_body) = req.into_parts();

    let mut redirect_uri = aws_runtime_addr.into_parts();
    redirect_uri.path_and_query = Some(req_parts.uri.path_and_query().unwrap().clone());
    let new_uri = Uri::from_parts(redirect_uri).unwrap();

    let request_body_waited = hyper::body::to_bytes(req_body).await?;

    let redirected_request = Request::builder()
        .method(req_parts.method.clone())
        .uri(new_uri)
        .body(Body::from(request_body_waited.clone()))
        .unwrap_or_else(|e| {
            error!("Error building redirected request: {}", e);
            Request::new(Body::empty())
        });

    // request forwarded to AWS runtime API
    let response = client.request(redirected_request).await?;

    match (req_parts.method, req_parts.uri.path()) {
        (hyper::Method::GET, "/2018-06-01/runtime/invocation/next") => {
            // case invocation/next, the *response body* contains the payload of
            // the request that the lambda handler will see
            let (resp_part, resp_body) = response.into_parts();
            let resp_payload = hyper::body::to_bytes(resp_body).await?;

            debug!("body before headers {:?}", resp_payload.clone());
            let body_with_headers = invoke_universal_instrumentation_start(
                Arc::clone(&span_generator),
                Arc::clone(&processor),
                resp_payload.clone(),
            )
            .await;

            debug!("Body with headers {}", body_with_headers);

            // Response is not cloneable, so it must be built again
            let mut rebuild_response = Response::builder()
                .status(resp_part.status)
                .version(resp_part.version)
                .body(Body::from(serde_json::to_vec(&body_with_headers).unwrap()))
                .unwrap();
            debug!("top resp headers {:?}", resp_part.headers);

            *rebuild_response.headers_mut() = resp_part.headers.clone();
            rebuild_response.headers_mut().insert(
                "content-length",
                format!("{}", body_with_headers.to_string().len())
                    .parse()
                    .unwrap(),
            );

            // complete forwarding to the lambda handler
            Ok(rebuild_response)
        }
        (hyper::Method::POST, path)
            if path.starts_with("/2018-06-01/runtime/invocation/")
                && path.ends_with("/response") =>
        {
            // case response to invocation, the *request* contains the returned
            // values and headers from lambda handler
            let parsed_body = serde_json::from_slice::<Value>(&request_body_waited);
            debug!("AG end invocation body {:?}", parsed_body);
            debug!("AG end invocation headers {:?}", req_parts.headers.clone());
            crate::lifecycle::listener::trace_invocation_end(
                processor.clone(),
                req_parts.headers.clone(),
                parsed_body,
            )
            .await;
            // only parsing of the original request (handler -> runtime API) is needed so
            // the original response can be used
            Ok(response)
        }
        _ => Ok(response),
    }
}

async fn invoke_universal_instrumentation_start(
    trace_processor: Arc<Mutex<impl TraceProcessor>>,
    processor: Arc<Mutex<Processor>>,
    resp_body: Bytes,
) -> Value {
    let mut req_wrapper_in_resp_body = deserialize_json(Ok(resp_body.clone())).unwrap();

    let headers = req_wrapper_in_resp_body.get("headers").unwrap();
    let mut headers_map: HashMap<String, String> = headers
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();

    if !headers_map.contains_key("x-datadog-trace-id") {
        let trace_id: u64 = random();
        debug!(
            "AG: No trace id found in headers, generating new one {}",
            trace_id
        );
        headers_map.insert("x-datadog-trace-id".to_string(), trace_id.to_string());
    }

    let mut existing_parent = 0;
    if headers_map.contains_key("x-datadog-parent-id") {
        existing_parent = headers_map
            .get("x-datadog-parent-id")
            .unwrap()
            .parse()
            .unwrap();
        debug!("AG: Found parent id in headers {}", existing_parent);
    }
    let aws_lambda_span_id: u64 = random();
    headers_map.insert(
        "x-datadog-parent-id".to_string(),
        aws_lambda_span_id.to_string(),
    );

    crate::lifecycle::listener::trace_invocation_start(
        Arc::clone(&processor),
        headers_map.clone(),
        resp_body.clone(),
    )
    .await;

    trace_processor
        .lock()
        .await
        .override_ids(0, existing_parent, aws_lambda_span_id);

    if let Some(headers) = req_wrapper_in_resp_body
        .get_mut("headers")
        .and_then(|h| h.as_object_mut())
    {
        *headers = serde_json::to_value(&headers_map)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();
    }
    req_wrapper_in_resp_body
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
