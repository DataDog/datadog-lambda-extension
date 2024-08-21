// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::service::{make_service_fn, service_fn};
use hyper::{http, Body, Method, Request, Response, StatusCode};
use serde_json::json;
use tracing::{error, warn};

use crate::tags::provider;

const HELLO_PATH: &str = "/lambda/hello";
const START_INVOCATION_PATH: &str = "/lambda/start-invocation";
const END_INVOCATION_PATH: &str = "/lambda/end-invocation";
const AGENT_PORT: usize = 8124;

pub struct Listener {
    pub tags_provider: Arc<provider::Provider>,
}

impl Listener {
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let make_svc = make_service_fn(move |_| {
            let service = service_fn(Self::handler);

            async move { Ok::<_, Infallible>(service) }
        });

        let port = u16::try_from(AGENT_PORT).expect("AGENT_PORT is too large");
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let server_builder = hyper::Server::try_bind(&addr)?;

        let server = server_builder.serve(make_svc);

        // start hyper http server
        if let Err(e) = server.await {
            error!("Failed to start the Lifecycle Listener {e}");
            return Err(e.into());
        }

        Ok(())
    }

    #[allow(clippy::unused_async)]
    async fn handler(req: Request<Body>) -> http::Result<Response<Body>> {
        match (req.method(), req.uri().path()) {
            (&Method::POST, START_INVOCATION_PATH) => Self::start_invocation_handler(req),
            (&Method::POST, END_INVOCATION_PATH) => Self::end_invocation_handler(req),
            (&Method::GET, HELLO_PATH) => Self::hello_handler(),
            _ => {
                let mut not_found = Response::default();
                *not_found.status_mut() = StatusCode::NOT_FOUND;
                Ok(not_found)
            }
        }
    }

    fn start_invocation_handler(_: Request<Body>) -> http::Result<Response<Body>> {
        Response::builder()
            .status(200)
            .body(Body::from(json!({}).to_string()))
    }

    fn end_invocation_handler(_: Request<Body>) -> http::Result<Response<Body>> {
        Response::builder()
            .status(200)
            .body(Body::from(json!({}).to_string()))
    }

    fn hello_handler() -> http::Result<Response<Body>> {
        warn!("[DEPRECATED] Please upgrade your tracing library, the /hello route is deprecated");
        Response::builder()
            .status(200)
            .body(Body::from(json!({}).to_string()))
    }
}
