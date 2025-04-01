use crate::telemetry::events::TelemetryEvent;

use ddcommon::hyper_migration;
use std::net::SocketAddr;
use tokio::sync::mpsc::Sender;

use http_body_util::BodyExt;
use hyper::service::service_fn;
use hyper::Response;
use std::io;
use tracing::{debug, error};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Copy)]
pub struct TelemetryListener {}

pub struct TelemetryListenerConfig {
    pub host: String,
    pub port: u16,
}

impl TelemetryListener {
    pub async fn spin(
        config: &TelemetryListenerConfig,
        event_bus: Sender<TelemetryEvent>,
        _cancel_token: tokio_util::sync::CancellationToken, // todo cancel token
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from(([0, 0, 0, 0], config.port));

        let service = service_fn(move |req| {
            let event_bus = event_bus.clone();
            Self::handle(req.map(hyper_migration::Body::incoming), event_bus.clone())
        });

        let listener = tokio::net::TcpListener::bind(&addr).await?;

        let server = hyper::server::conn::http1::Builder::new();
        let mut joinset = tokio::task::JoinSet::new();
        loop {
            let conn = tokio::select! {
                con_res = listener.accept() => match con_res {
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionRefused
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => {
                        error!("Server error: {e}");
                        return Err(e.into());
                    }
                    Ok((conn, _)) => conn,
                },
                finished = async {
                    match joinset.join_next().await {
                        Some(finished) => finished,
                        None => std::future::pending().await,
                    }
                } => match finished {
                    Err(e) if e.is_panic() => {
                        std::panic::resume_unwind(e.into_panic());
                    },
                    Ok(()) | Err(_) => continue,
                },
            };
            let conn = hyper_util::rt::TokioIo::new(conn);
            let server = server.clone();
            let service = service.clone();
            joinset.spawn(async move {
                if let Err(e) = server.serve_connection(conn, service).await {
                    error!("Connection error: {e}");
                }
            });
        }
    }

    pub async fn handle(
        req: hyper_migration::HttpRequest,
        event_bus: Sender<TelemetryEvent>,
    ) -> Result<hyper_migration::HttpResponse, hyper::Error> {
        let hyper_migration::Body::Incoming(body_bytes) = req.into_body() else {
            unimplemented!()
        };
        let body = match body_bytes.collect().await {
            Ok(body_bytes_collected) => body_bytes_collected.to_bytes().to_vec(),
            Err(e) => {
                error!("Failed to collect body: {:?}", e);
                return Ok(Response::builder()
                    .status(hyper::StatusCode::INTERNAL_SERVER_ERROR)
                    .body(hyper_migration::Body::from("Failed to collect body"))
                    .expect("infallible"));
            }
        };
        let body = std::str::from_utf8(&body).expect("infallible");

        let mut telemetry_events: Vec<TelemetryEvent> = match serde_json::from_str(body) {
            Ok(events) => events,
            Err(e) => {
                // If we can't parse the event, we will receive it again in a new batch
                // causing an infinite loop and resource contention.
                // Instead, log it and move on.
                // This will result in a dropped payload, but may be from
                // events we haven't added support for yet
                debug!("Failed to parse telemetry events: {:?}", e);
                return Ok(Response::builder()
                    .status(hyper::StatusCode::OK)
                    .body(hyper_migration::Body::from(
                        "Failed to parse telemetry events",
                    ))
                    .expect("infallible"));
            }
        };
        for event in telemetry_events.drain(..) {
            event_bus.send(event).await.expect("infallible");
        }

        Ok(Response::new(hyper_migration::Body::from("OK")))
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use ddcommon::hyper_migration;

    use crate::telemetry::events::{InitPhase, InitType, TelemetryRecord};

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_handle() {
        let event_body = hyper_migration::Body::from(
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
