use hyper::body::Bytes;
use hyper::client::HttpConnector;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Error, Request, Response, Server, Uri};
use hyper_proxy::{Intercept, Proxy, ProxyConnector};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{debug, error};

#[must_use]
pub fn start_lwa_proxy() -> Option<JoinHandle<()>> {
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
                    let uri = aws_runtime_uri.clone();
                    let client = Arc::clone(&proxied_client);
                    async move {
                        Ok::<_, hyper::Error>(service_fn(move |req| {
                            inject_spans(req, Arc::clone(&client), uri.clone())
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
    mut req: Request<Body>,
    client: Arc<Client<ProxyConnector<HttpConnector>>>,
    aws_runtime_addr: Uri,
) -> Result<Response<Body>, Error> {
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

    let mut parts = aws_runtime_addr.into_parts();
    parts.path_and_query = req.uri().path_and_query().cloned();
    let new_uri = Uri::from_parts(parts).unwrap();
    debug!(
        "Injecting spans into request. Intercepted uri: {} and substituting authority with uri: {}",
        req.uri(),
        new_uri
    );

    *req.uri_mut() = new_uri;

    let mut response = client.request(req).await?;
    let body_bytes = deserialize_json(hyper::body::to_bytes(response.body_mut()).await).unwrap();
    let body_with_added_headers =
        add_headers(body_bytes.clone(), "x-test-header", "val").to_string();

    let new_length = body_with_added_headers.len();

    let mut new_response = Response::builder()
        .status(response.status())
        .version(response.version())
        .body(Body::from(body_with_added_headers))
        .unwrap();

    *new_response.headers_mut() = response.headers().clone();
    new_response
        .headers_mut()
        .insert("Content-Length", new_length.to_string().parse().unwrap());

    debug!("Response after header injections: {:?}", new_response);

    Ok(new_response)
}

fn add_headers(mut body_json: Value, header_name: &str, header_value: &str) -> Value {
    body_json
        .get_mut("headers")
        .and_then(|headers| headers.as_object_mut())
        .map(|headers| {
            headers.insert(
                header_name.to_string(),
                Value::String(header_value.to_string()),
            );
        });
    body_json
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

    #[tokio::test]
    async fn test_proxy_with_env_vars() {
        let proxy_uri = "127.0.0.1:12345";
        let final_uri = "127.0.0.1:12344";

        env::set_var("ALTERNATE_RUNTIME_API", proxy_uri);
        env::set_var("AWS_LAMBDA_RUNTIME_API", final_uri);

        let final_destination = tokio::spawn(async {
            hyper::Server::bind(&SocketAddr::from(([127, 0, 0, 1], 12344)))
                .serve(make_service_fn(|_| async {
                    Ok::<_, hyper::Error>(service_fn(|_| async {
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

        assert!(ask_proxy.headers().contains_key("x-test-header"));
        assert_eq!(ask_proxy.headers().get("x-test-header").unwrap(), "abc");

        let body_bytes = hyper::body::to_bytes(ask_proxy.into_body()).await.unwrap();
        let bytes = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(bytes, "Response from AWS LAMBDA RUNTIME API");

        proxy_task_handle.abort();
        final_destination.abort();

        env::remove_var("ALTERNATE_RUNTIME_API");
        env::remove_var("AWS_LAMBDA_RUNTIME_API");
    }

    #[test]
    fn inject_headers_to_body() {
        let example_payload = r#"
    {
        "version": "2.0",
        "routeKey": "$default",
        "rawPath": "/",
        "rawQueryString": "",
        "headers": {
            "sec-fetch-mode": "navigate",
            "x-amzn-tls-version": "TLSv1.3",
            "if-none-match": "W/\"73-nZ1afAshmFkchuWGJW2eF+mD4HM\"",
            "sec-fetch-site": "cross-site",
            "x-forwarded-proto": "https",
            "accept-language": "en-US,en;q=0.9,ha;q=0.8",
            "x-forwarded-port": "443",
            "x-forwarded-for": "64.124.12.18",
            "sec-fetch-user": "?1",
            "accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
            "x-amzn-tls-cipher-suite": "TLS_AES_128_GCM_SHA256",
            "sec-ch-ua": "\"Chromium\";v=\"130\", \"Google Chrome\";v=\"130\", \"Not?A_Brand\";v=\"99\"",
            "x-amzn-trace-id": "Root=1-67114ebe-7daee824361c14682e44cc57",
            "sec-ch-ua-mobile": "?0",
            "sec-ch-ua-platform": "\"Linux\"",
            "host": "nfaeiph7yxziojig2kl5u75r2m0vozse.lambda-url.us-east-1.on.aws",
            "upgrade-insecure-requests": "1",
            "cache-control": "max-age=0",
            "accept-encoding": "gzip, deflate, br, zstd",
            "user-agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
            "sec-fetch-dest": "document"
        },
        "requestContext": {
            "accountId": "anonymous",
            "apiId": "nfaeiph7yxziojig2kl5u75r2m0vozse",
            "domainName": "nfaeiph7yxziojig2kl5u75r2m0vozse.lambda-url.us-east-1.on.aws",
            "domainPrefix": "nfaeiph7yxziojig2kl5u75r2m0vozse",
            "http": {
                "method": "GET",
                "path": "/",
                "protocol": "HTTP/1.1",
                "sourceIp": "64.124.12.18",
                "userAgent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36"
            },
            "requestId": "c6637907-97d0-4ebc-8c90-37adc31161be",
            "routeKey": "$default",
            "stage": "$default",
            "time": "17/Oct/2024:17:51:58 +0000",
            "timeEpoch": 1729187518389
        },
        "isBase64Encoded": false
    }
    "#;

        let res = deserialize_json(Ok(Bytes::from(example_payload.as_bytes()))).unwrap();
        println!("{:?}", res);

        let res2 = add_headers(res.clone(), "x-test-header", "val");
        println!("{:?}", res2);

        assert_eq!(
            res2["headers"]["x-test-header"],
            Value::String("val".to_string())
        );
    }
}
