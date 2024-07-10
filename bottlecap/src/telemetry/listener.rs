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

        let mut telemetry_events: Vec<TelemetryEvent> = match serde_json::from_str(body) {
            Ok(events) => events,
            Err(e) => {
                // If we can't parse the event, we will receive it again in a new batch
                // causing an infinite loop and resource contention.
                // Instead, log it as fatal and move on.
                // This will result in a dropped payload.
                error!("[FATAL] Failed to parse telemetry events: {:?}", e);
                return Ok(Response::builder()
                    .status(hyper::StatusCode::OK)
                    .body(Body::from("Failed to parse telemetry events"))
                    .expect("infallible"));
            }
        };
        for event in telemetry_events.drain(..) {
            event_bus.send(event).await.expect("infallible");
        }

        let ack = Response::new(Body::from("OK"));
        Ok(ack)
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use crate::telemetry::events::{InitPhase, InitType, TelemetryRecord};

    #[tokio::test]
    async fn test_handle() {
        let event_body = hyper::Body::from(
            r#"[{"time":"2024-04-25T17:35:59.944Z","type":"platform.initStart","record":{"initializationType":"on-demand","phase":"init","runtimeVersion":"nodejs:20.v22","runtimeVersionArn":"arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72","functionName":"hello-world","functionVersion":"$LATEST"}}]"#,
        );
        let req = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost:8080")
            .body(event_body)
            .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let _ = super::TelemetryListener::handle(req, tx).await;

        let telemetry_event = rx.recv().await.unwrap();
        let expected_time =
            DateTime::parse_from_rfc3339("2024-04-25T17:35:59.944Z").expect("failed to parse time");
        assert_eq!(telemetry_event.time, expected_time);
        assert_eq!(telemetry_event.record, TelemetryRecord::PlatformInitStart {
            initialization_type: InitType::OnDemand,
            phase: InitPhase::Init,
            runtime_version: Some("nodejs:20.v22".to_string()),
            runtime_version_arn: Some("arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72".to_string()),
        });
    }
}
