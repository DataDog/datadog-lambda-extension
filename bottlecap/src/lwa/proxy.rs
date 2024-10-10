use hyper_proxy::{Intercept, Proxy, ProxyConnector};

use hyper::client::HttpConnector;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Request, Response, Server, Uri};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::error;

pub fn start_lwa_proxy() -> Option<JoinHandle<()>> {
    if let Some(config) = parse_env_addresses() {
        println!("Proxy URI: {}", config.proxy_uri);
        println!("Proxy Socket Address: {}", config.proxy_socket_addr);
        println!("AWS Runtime Address: {}", config.aws_runtime_addr);
        let proxy = build_proxy(config.proxy_uri.clone())?;
        let client = Arc::new(Client::builder().build::<_, Body>(proxy));

        let proxy_task_handle = tokio::spawn({
            let client = client.clone();
            async move {
                let proxy_server =
                    Server::bind(&config.proxy_socket_addr).serve(make_service_fn(move |_| {
                        let uri = config.aws_runtime_addr.clone();
                        let client = client.clone();
                        async move {
                            Ok::<_, hyper::Error>(service_fn(move |req| {
                                inject_spans(req, client.clone(), uri.clone())
                            }))
                        }
                    }));

                if let Err(e) = proxy_server.await {
                    error!("Lambda Web Adapter proxy server error: {}", e);
                }
            }
        });
        Some(proxy_task_handle)
    } else {
        None
    }
}

fn build_proxy(proxy_uri: Uri) -> Option<ProxyConnector<HttpConnector>> {
    let proxy = {
        match ProxyConnector::from_proxy(
            HttpConnector::new(),
            Proxy::new(Intercept::All, "http://127.0.0.1:12344".parse().unwrap()),
            // Proxy::new(Intercept::All, proxy_uri),
        ) {
            Ok(proxy_connector) => proxy_connector,
            Err(e) => {
                error!("Error creating proxy connector: {}", e);
                return None;
            }
        }
    };
    Some(proxy)
}

struct ProxyConfig {
    proxy_uri: Uri,
    proxy_socket_addr: SocketAddr,
    aws_runtime_addr: Uri,
}

fn parse_env_addresses() -> Option<ProxyConfig> {
    let alternate_runtime_api: Option<(Uri, SocketAddr)> =
        match std::env::var("ALTERNATE_RUNTIME_API") {
            Ok(uri) => match uri.parse::<Uri>() {
                Ok(parsed_uri) => {
                    let host = parsed_uri.host()?;
                    let port = parsed_uri.port_u16()?;
                    let socket_addr = format!("{}:{}", host, port).parse::<SocketAddr>().ok()?;
                    Some((parsed_uri, socket_addr))
                }
                Err(e) => {
                    error!("Error parsing ALTERNATE_RUNTIME_API: {}", e);
                    None
                }
            },
            Err(e) => {
                error!("Error retrieving ALTERNATE_RUNTIME_API: {}", e);
                None
            }
        };

    let aws_runtime_api: Option<Uri> = match std::env::var("AWS_LAMBDA_RUNTIME_API") {
        Ok(uri) => match uri.parse() {
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
        (Some(proxy_uri), Some(aws_runtime_addr)) => Some(ProxyConfig {
            proxy_uri: proxy_uri.0,
            proxy_socket_addr: proxy_uri.1,
            aws_runtime_addr,
        }),
        _ => None,
    }
}

async fn inject_spans(
    mut req: Request<Body>,
    client: Arc<Client<ProxyConnector<HttpConnector>>>,
    aws_runtime_addr: Uri,
) -> Result<Response<Body>, hyper::Error> {
    // let tracer_getter_for_version = match (req.method(), req.uri().path()) {
    //     (&Method::PUT | &Method::POST, V4_TRACE_ENDPOINT_PATH) => get_traces_from_request_body,
    //     (&Method::PUT | &Method::POST, V5_TRACE_ENDPOINT_PATH) => get_v05_traces_from_request_body,
    // };

    // let (body_size, traces) = match tracer_getter_for_version(req.into_body()).await {
    //     Ok(res) => res,
    //     Err(err) => {
    //         return log_and_create_http_response(
    //             &format!("Error deserializing trace from request body: {err}"),
    //             StatusCode::INTERNAL_SERVER_ERROR,
    //         )
    //     }
    // };

    *req.uri_mut() = aws_runtime_addr;

    // let req = Request::get(aws_runtime_addr.clone())
    //     .body(Body::empty())
    //     .unwrap();

    // let response = client.request(req).await?;
    let mut response = client
        .get("http://127.0.0.1:12344".parse().unwrap())
        .await?;
    // let response = match client.get("http://127.0.0.1:12344".parse().unwrap()).await {

    response.headers_mut().insert(
        "x-test-header",
        hyper::header::HeaderValue::from_static("abc"),
    );

    println!("Request URI: {}", req.uri());
    println!("Response: {:?}", response);
    // let response = match client.request(req).await {
    //     Ok(res) => res,
    //     Err(e) => {
    //         println!("Error making request to AWS Lambda Runtime API: {}", e);
    //         return Ok(Response::new(Body::empty()));
    //     }
    // };

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::Uri;
    use std::env;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_proxy_with_env_vars() {
        let proxy_uri = "http://127.0.0.1:12345";
        let final_uri = "http://127.0.0.1:12344";

        env::set_var("ALTERNATE_RUNTIME_API", proxy_uri);
        env::set_var("AWS_LAMBDA_RUNTIME_API", final_uri);

        let final_destination = tokio::spawn(async {
            hyper::Server::bind(&SocketAddr::from(([127, 0, 0, 1], 12344)))
                .serve(make_service_fn(|_| async {
                    Ok::<_, hyper::Error>(service_fn(|req| async move {
                        Ok::<_, hyper::Error>(Response::new(hyper::Body::from(
                            "Response from AWS LAMBDA RUNTIME API",
                        )))
                    }))
                }))
                .await
                .unwrap();
        });

        let proxy_task_handle = start_lwa_proxy().expect("Failed to start proxy");

        let client = Client::builder().build_http::<Body>();
        let mut ask_proxy = client.get(Uri::from_static(proxy_uri)).await;

        while ask_proxy.is_err() {
            println!("Retrying request to proxy");
            sleep(Duration::from_millis(100)).await;
            ask_proxy = client.get(Uri::from_static(proxy_uri)).await;
        }

        let ask_proxy = ask_proxy.unwrap();

        assert!(ask_proxy.headers().contains_key("x-test-header"));
        assert_eq!(ask_proxy.headers().get("x-test-header").unwrap(), "abc");

        // Read the response body
        let body_bytes = hyper::body::to_bytes(ask_proxy.into_body()).await.unwrap();
        let bytes = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(bytes, "Response from AWS LAMBDA RUNTIME API");

        // Clean up
        proxy_task_handle.abort();
        final_destination.abort();

        env::remove_var("ALTERNATE_RUNTIME_API");
        env::remove_var("AWS_LAMBDA_RUNTIME_API");
    }
}
