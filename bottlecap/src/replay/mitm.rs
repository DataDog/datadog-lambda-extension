//! An HTTP(S) proxy that terminates TLS with certificates minted by the
//! replay CA and hands each request to a [`Handler`]. Record mode forwards
//! upstream; replay mode answers from the recording.
//!
//! Clients reach it through `HTTP(S)_PROXY`: HTTPS arrives as `CONNECT`,
//! plain HTTP as absolute-form requests.

use crate::replay::ca::CertificateAuthority;
use crate::replay::format::{Body, HttpRequest, HttpResponse, headers_from};
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tracing::debug;

pub type HandlerFuture = Pin<Box<dyn Future<Output = HttpResponse> + Send>>;
pub type Handler = Arc<dyn Fn(HttpRequest) -> HandlerFuture + Send + Sync>;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub async fn serve(
    listener: TcpListener,
    ca: Arc<CertificateAuthority>,
    handler: Handler,
    shutdown: CancellationToken,
) {
    loop {
        let stream = tokio::select! {
            () = shutdown.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    debug!("REPLAY | MITM | accept failed: {e}");
                    continue;
                }
            },
        };
        let ca = Arc::clone(&ca);
        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            let svc = service_fn(move |req| {
                let ca = Arc::clone(&ca);
                let handler = Arc::clone(&handler);
                async move { Ok::<_, Infallible>(proxy(req, ca, handler).await) }
            });
            if let Err(e) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .with_upgrades()
                .await
            {
                debug!("REPLAY | MITM | connection error: {e}");
            }
        });
    }
}

async fn proxy(
    req: Request<Incoming>,
    ca: Arc<CertificateAuthority>,
    handler: Handler,
) -> Response<Full<Bytes>> {
    if req.method() != Method::CONNECT {
        // Absolute-form plain HTTP request.
        return handle(req, None, handler).await;
    }
    let Some(authority) = req.uri().authority().cloned() else {
        return status_response(StatusCode::BAD_REQUEST, "CONNECT without authority");
    };
    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => {
                debug!("REPLAY | MITM | upgrade failed: {e}");
                return;
            }
        };
        let host = authority.host().to_string();
        let config = match ca.server_config(&host) {
            Ok(c) => c,
            Err(e) => {
                debug!("REPLAY | MITM | no certificate for {host}: {e}");
                return;
            }
        };
        let tls = match TlsAcceptor::from(config)
            .accept(TokioIo::new(upgraded))
            .await
        {
            Ok(t) => t,
            Err(e) => {
                debug!("REPLAY | MITM | TLS handshake with client for {host} failed: {e}");
                return;
            }
        };
        let base = match authority.port_u16() {
            Some(443) | None => format!("https://{host}"),
            Some(port) => format!("https://{host}:{port}"),
        };
        let svc = service_fn(move |req| {
            let handler = Arc::clone(&handler);
            let base = base.clone();
            async move { Ok::<_, Infallible>(handle(req, Some(base), handler).await) }
        });
        if let Err(e) = http1::Builder::new()
            .serve_connection(TokioIo::new(tls), svc)
            .await
        {
            debug!("REPLAY | MITM | tunnel error for {host}: {e}");
        }
    });
    Response::new(Full::default())
}

/// Buffers the request, asks the handler, and writes its response back.
/// `base` is `https://host[:port]` inside a tunnel.
async fn handle(
    req: Request<Incoming>,
    base: Option<String>,
    handler: Handler,
) -> Response<Full<Bytes>> {
    let (parts, body) = req.into_parts();
    let bytes = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => return status_response(StatusCode::BAD_REQUEST, &format!("bad body: {e}")),
    };
    let url = match base {
        Some(b) => format!(
            "{b}{}",
            parts.uri.path_and_query().map_or("/", |p| p.as_str())
        ),
        None => parts.uri.to_string(),
    };
    let request = HttpRequest {
        method: parts.method.to_string(),
        url,
        headers: headers_from(&parts.headers, false),
        body: Body::from_bytes(&bytes),
    };
    to_hyper_response(&handler(request).await)
}

fn to_hyper_response(resp: &HttpResponse) -> Response<Full<Bytes>> {
    let mut builder = Response::builder().status(resp.status);
    for (name, values) in &resp.headers {
        if HOP_BY_HOP.contains(&name.as_str()) || name == "content-length" {
            continue;
        }
        for v in values {
            builder = builder.header(name, v);
        }
    }
    builder
        .body(Full::new(Bytes::from(resp.body.to_bytes())))
        .unwrap_or_else(|e| status_response(StatusCode::BAD_GATEWAY, &e.to_string()))
}

fn status_response(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(Bytes::from(msg.to_string())));
    *r.status_mut() = status;
    r
}

/// Sends `req` upstream and returns the response. A transport failure
/// becomes a `502` so it is recorded and replayed like any other answer.
pub async fn forward(client: &reqwest::Client, req: &HttpRequest) -> HttpResponse {
    let method = reqwest::Method::from_bytes(req.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut rb = client.request(method, &req.url);
    for (name, values) in &req.headers {
        if HOP_BY_HOP.contains(&name.as_str()) || name == "host" || name == "content-length" {
            continue;
        }
        for v in values {
            rb = rb.header(name, v);
        }
    }
    match rb.body(req.body.to_bytes()).send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let headers = headers_from(r.headers(), false);
            let body = r.bytes().await.unwrap_or_default();
            HttpResponse {
                status,
                headers,
                body: Body::from_bytes(&body),
            }
        }
        Err(e) => HttpResponse {
            status: 502,
            headers: [("x-replay-error".to_string(), vec!["upstream".to_string()])].into(),
            body: Body::from_bytes(format!("upstream request failed: {e}").as_bytes()),
        },
    }
}

/// A handler that forwards upstream and reports every exchange.
pub fn forwarding_handler<F>(client: reqwest::Client, on_exchange: F) -> Handler
where
    F: Fn(HttpRequest, HttpResponse, Duration) + Send + Sync + 'static,
{
    let on_exchange = Arc::new(on_exchange);
    Arc::new(move |req: HttpRequest| {
        let client = client.clone();
        let on_exchange = Arc::clone(&on_exchange);
        Box::pin(async move {
            let started = Instant::now();
            let resp = forward(&client, &req).await;
            on_exchange(req, resp.clone(), started.elapsed());
            resp
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A client that trusts only the replay CA, through the proxy, gets the
    /// handler's answer and the handler sees the decrypted request.
    #[tokio::test]
    async fn https_through_connect_reaches_handler() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = Arc::new(CertificateAuthority::generate().expect("ca"));
        let seen: Arc<Mutex<Vec<HttpRequest>>> = Arc::default();
        let seen_clone = Arc::clone(&seen);
        let handler: Handler = Arc::new(move |req: HttpRequest| {
            seen_clone.lock().expect("lock").push(req);
            Box::pin(async {
                HttpResponse {
                    status: 201,
                    headers: [("x-test".to_string(), vec!["yes".to_string()])].into(),
                    body: Body::from_bytes(b"hello"),
                }
            })
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let shutdown = CancellationToken::new();
        tokio::spawn(serve(listener, Arc::clone(&ca), handler, shutdown.clone()));

        let client = datadog_fips::reqwest_adapter::create_reqwest_client_builder()
            .expect("builder")
            .tls_built_in_root_certs(false)
            .add_root_certificate(
                reqwest::Certificate::from_pem(ca.cert_pem().as_bytes()).expect("ca pem"),
            )
            .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).expect("proxy"))
            .build()
            .expect("client");
        let resp = client
            .post("https://example.invalid/path?q=1")
            .body("payload")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status().as_u16(), 201);
        assert_eq!(resp.headers()["x-test"], "yes");
        assert_eq!(resp.text().await.expect("text"), "hello");

        let seen = seen.lock().expect("lock").clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].url, "https://example.invalid/path?q=1");
        assert_eq!(seen[0].body.text.as_deref(), Some("payload"));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn plain_http_absolute_form() {
        let ca = Arc::new(CertificateAuthority::generate().expect("ca"));
        let handler: Handler = Arc::new(|req: HttpRequest| {
            Box::pin(async move {
                HttpResponse {
                    status: 200,
                    headers: std::collections::BTreeMap::default(),
                    body: Body::from_bytes(req.url.as_bytes()),
                }
            })
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(serve(listener, ca, handler, CancellationToken::new()));

        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        s.write_all(b"GET http://example.invalid/x HTTP/1.1\r\nHost: example.invalid\r\nConnection: close\r\n\r\n")
            .await
            .expect("write");
        let mut out = String::new();
        s.read_to_string(&mut out).await.expect("read");
        assert!(out.ends_with("http://example.invalid/x"), "{out}");
    }
}
