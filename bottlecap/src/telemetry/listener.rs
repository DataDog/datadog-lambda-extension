use crate::telemetry::events::TelemetryEvent;

use std::net::SocketAddr;
use tokio::sync::mpsc::Sender;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use tracing::{debug, error};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Copy)]
pub struct TelemetryListener {}

pub struct TelemetryListenerConfig {
    pub host: String,
    pub port: u16,
}

impl TelemetryListener {
    pub async fn new_hyper(
        config: &TelemetryListenerConfig,
        event_bus: Sender<TelemetryEvent>,
        _cancel_token: tokio_util::sync::CancellationToken,
    ) {
        // let addr = format!("{}:{}", &config.host, &config.port);
        let addr = SocketAddr::from(([0, 0, 0, 0], config.port));

        let service = make_service_fn(move |_| {
            let event_bus = event_bus.clone();
            async move {
                Ok::<_, hyper::Error>(service_fn(move |req| Self::handle(req, event_bus.clone())))
            }
        });
        let listener = Server::bind(&addr);

        let server = listener.serve(service);
        debug!("Starting Telemetry API listener on {}", addr);
        if let Err(e) = server.await {
            error!("Telemetry API listener error: {:?}", e);
        }
    }

    pub async fn handle(
        req: Request<Body>,
        event_bus: Sender<TelemetryEvent>,
    ) -> Result<Response<Body>, hyper::Error> {
        let whole_body = hyper::body::to_bytes(req.into_body()).await?;

        let body = whole_body.to_vec();
        let body = std::str::from_utf8(&body).expect("infallible");

        let mut telemetry_events: Vec<TelemetryEvent> = serde_json::from_str(body).expect("infallible");
        for event in telemetry_events.drain(..) {
            event_bus.send(event).await.expect("infallible");
        }

        let ack = Response::new(Body::from("OK"));
        Ok(ack)
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_handle() {
        let req = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost:8080")
            .body(hyper::Body::from(r#"{"key": "value"}"#))
            .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let _ = super::TelemetryListener::handle(req, tx).await;

        let events = rx.recv().await.unwrap();
        assert_eq!(events.len(), 1);
        //assert_eq!(events[0].get("key").unwrap(), "value");
    }
}
