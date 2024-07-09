// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// TODO(Astuyve): Deprecate.
// older clients require the 127.0.0.1:8126/lambda/hello route
// to identify the presence of the extension.

use hyper::service::{make_service_fn, service_fn};
use hyper::{http, Body, Method, Request, Response, Server, StatusCode};
use serde_json::json;
use std::convert::Infallible;
use std::net::SocketAddr;
use tracing::error;

const HELLO_PATH: &str = "/lambda/hello";
const AGENT_PORT: usize = 8124;

pub async fn start_handler() -> Result<(), Box<dyn std::error::Error>> {
    let make_svc = make_service_fn(move |_| {
        let service = service_fn(hello_handler);

        async move { Ok::<_, Infallible>(service) }
    });

    let port = u16::try_from(AGENT_PORT).expect("AGENT_PORT is too large");
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let server_builder = Server::try_bind(&addr)?;

    let server = server_builder.serve(make_svc);

    // start hyper http server
    if let Err(e) = server.await {
        error!("Server error: {e}");
        return Err(e.into());
    }

    Ok(())
}

async fn hello_handler(req: Request<Body>) -> http::Result<Response<Body>> {
    if let (&Method::GET, HELLO_PATH) = (req.method(), req.uri().path()) {
        Response::builder()
            .status(200)
            .body(Body::from(json!({}).to_string()))
    } else {
        let mut not_found = Response::default();
        *not_found.status_mut() = StatusCode::NOT_FOUND;
        Ok(not_found)
    }
}
