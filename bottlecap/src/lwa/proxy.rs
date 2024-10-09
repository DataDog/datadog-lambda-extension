use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use std::net::SocketAddr;
use tokio::task::JoinHandle;

pub fn start_lwa_proxy() -> JoinHandle<()> {
    let proxy_task_handle = tokio::spawn(async {
        let addr = SocketAddr::from(([0, 0, 0, 0], 12345));
        let proxy_server = Server::bind(&addr).serve(make_service_fn(|_| async {
            Ok::<_, hyper::Error>(service_fn(proxy))
        }));

        if let Err(e) = proxy_server.await {
            println!("LWA proxy server error: {}", e);
        }
    });

    proxy_task_handle
}

async fn proxy(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    Ok(Response::new(Body::from("Hello, World!")))

    // let client = hyper::Client::new();
    // add spans
    // client.request(req).await
}

#[cfg(test)]
mod tests {
    use crate::lwa::proxy::start_lwa_proxy;
    use hyper::Uri;

    #[tokio::test]
    async fn proxy_test() {
        let _proxy_task_handle = start_lwa_proxy();

        let resp_body = hyper::Client::new()
            .get(Uri::from_static("http://localhost:12345"))
            .await
            .unwrap();

        let body_bytes = hyper::body::to_bytes(resp_body.into_body()).await.unwrap();
        let bytes = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert_eq!(bytes, "Hello, World!");
    }
}
