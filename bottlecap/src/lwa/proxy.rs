use crate::lifecycle::invocation::processor::Processor;
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
pub fn start_lwa_proxy(span_generator: Arc<Mutex<Processor>>) -> Option<JoinHandle<()>> {
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
                    let arc = Arc::clone(&span_generator);
                    let uri = aws_runtime_uri.clone();
                    let client = Arc::clone(&proxied_client);
                    async move {
                        Ok::<_, hyper::Error>(service_fn(move |req| {
                            inject_spans(req, Arc::clone(&client), Arc::clone(&arc), uri.clone())
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
    let alternate_runtime_api = match std::env::var("ALTERNATE_RUNTIME_API") {
        Ok(uri) => match uri.parse::<Uri>() {
            Ok(parsed_uri) => {
                let host = parsed_uri.host()?;
                let port = parsed_uri.port_u16()?;
                let socket_addr = format!("{host}:{port}").parse::<SocketAddr>().ok()?;
                Some(socket_addr)
            }
            Err(e) => {
                error!("Error parsing ALTERNATE_RUNTIME_API: {}", e);
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
    match (alternate_runtime_api, aws_runtime_api) {
        (Some(proxy_uri), Some(aws_runtime_addr)) => Some((proxy_uri, aws_runtime_addr)),
        _ => None,
    }
}

async fn inject_spans(
    req: Request<Body>,
    client: Arc<Client<ProxyConnector<HttpConnector>>>,
    span_generator: Arc<Mutex<Processor>>,
    aws_runtime_addr: Uri,
) -> Result<Response<Body>, Error> {
    let (req_parts, req_body) = req.into_parts();

    let mut new_parts = aws_runtime_addr.into_parts();
    new_parts.path_and_query = Some(req_parts.uri.path_and_query().unwrap().clone());
    let new_uri = Uri::from_parts(new_parts).unwrap();
    debug!(
        "Intercepted uri: {} and substituting authority with uri: {}. Method {}",
        req_parts.uri, new_uri, req_parts.method
    );

    let req_payload = hyper::body::to_bytes(req_body).await?;

    let req = Request::builder()
        .method(req_parts.method.clone())
        .uri(new_uri)
        .body(Body::from(req_payload.clone()))
        .map_err(|e| {
            error!("Error building request: {}", e);
            e
        })
        .unwrap();

    let mut response = client.request(req).await?;

    let (resp_part, resp_body) = response.into_parts();
    let resp_payload = hyper::body::to_bytes(resp_body).await?;
    let resp_body = deserialize_json(Ok(resp_payload.clone())).unwrap();

    if req_parts.uri.path() == "/2018-06-01/runtime/invocation/next"
        && req_parts.method == hyper::Method::GET
    {
        debug!("Intercepted invocation request");

        let vec = serde_json::to_vec(&resp_body);
        if vec.is_ok() {
            span_generator
                .lock()
                .await
                .on_invocation_start(vec.unwrap());
        }
    } else if req_parts
        .uri
        .path()
        .starts_with("/2018-06-01/runtime/invocation/")
        && req_parts.uri.path().ends_with("/response")
        && req_parts.method == hyper::Method::POST
    {
        debug!("Intercepted invocation response {:?}", req_payload);
        span_generator
            .lock()
            .await
            .on_invocation_end(random(), random(), random(), None);
    }
    let mut response2 = Response::builder()
        .status(resp_part.status)
        .version(resp_part.version)
        .body(Body::from(resp_payload))
        .unwrap();

    *response2.headers_mut() = resp_part.headers;
    response = response2;
    Ok(response)
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
    use hyper::Uri;
    use std::env;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::time::sleep;

    // #[tokio::test]
    // async fn test_proxy_with_env_vars() {
    //     let proxy_uri = "127.0.0.1:12345";
    //     let final_uri = "127.0.0.1:12344";
    //
    //     env::set_var("ALTERNATE_RUNTIME_API", proxy_uri);
    //     env::set_var("AWS_LAMBDA_RUNTIME_API", final_uri);
    //
    //     let final_destination = tokio::spawn(async {
    //         hyper::Server::bind(&SocketAddr::from(([127, 0, 0, 1], 12344)))
    //             .serve(make_service_fn(|_| async {
    //                 Ok::<_, hyper::Error>(service_fn(|_| async {
    //                     Ok::<_, hyper::Error>(Response::new(hyper::Body::from(
    //                         "Response from AWS LAMBDA RUNTIME API",
    //                     )))
    //                 }))
    //             }))
    //             .await
    //             .unwrap();
    //     });
    //
    //     let proxy_task_handle = start_lwa_proxy().expect("Failed to start proxy");
    //
    //     let client = Client::builder().build_http::<Body>();
    //     let uri_with_schema = format!("http://{}", proxy_uri);
    //     let mut ask_proxy = client
    //         .get(Uri::try_from(uri_with_schema.clone()).unwrap())
    //         .await;
    //
    //     while ask_proxy.is_err() {
    //         println!(
    //             "Retrying request to proxy, err: {}",
    //             ask_proxy.err().unwrap()
    //         );
    //         sleep(Duration::from_millis(50)).await;
    //         ask_proxy = client
    //             .get(Uri::try_from(uri_with_schema.clone()).unwrap())
    //             .await;
    //     }
    //
    //     let ask_proxy = ask_proxy.unwrap();
    //
    //     assert!(ask_proxy.headers().contains_key("x-test-header"));
    //     assert_eq!(ask_proxy.headers().get("x-test-header").unwrap(), "abc");
    //
    //     let body_bytes = hyper::body::to_bytes(ask_proxy.into_body()).await.unwrap();
    //     let bytes = String::from_utf8(body_bytes.to_vec()).unwrap();
    //     assert_eq!(bytes, "Response from AWS LAMBDA RUNTIME API");
    //
    //     proxy_task_handle.abort();
    //     final_destination.abort();
    //
    //     env::remove_var("ALTERNATE_RUNTIME_API");
    //     env::remove_var("AWS_LAMBDA_RUNTIME_API");
    // }
}
