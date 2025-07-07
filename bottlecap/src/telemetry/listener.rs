use crate::{
    http::{extract_request_body, handler_not_found},
    telemetry::events::TelemetryEvent,
};

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tracing::debug;

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
        cancel_token: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
        let router = Self::make_router(event_bus);

        debug!("Telemetry API | Starting listener on {}", addr);
        let listener = TcpListener::bind(&addr).await?;

        axum::serve(listener, router)
            .with_graceful_shutdown(Self::graceful_shutdown(cancel_token))
            .await?;

        Ok(())
    }

    fn make_router(event_bus: Sender<TelemetryEvent>) -> Router {
        Router::new()
            .route("/", post(Self::handle))
            .fallback(handler_not_found)
            .with_state(event_bus)
    }

    async fn graceful_shutdown(cancel_token: CancellationToken) {
        cancel_token.cancelled().await;
        debug!("Telemetry API | Shutdown signal received, shutting down");
    }

    async fn handle(State(event_bus): State<Sender<TelemetryEvent>>, request: Request) -> Response {
        let (_, body) = match extract_request_body(request).await {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to extract request body: {e}"),
                )
                    .into_response();
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
                return (StatusCode::OK, "Failed to parse telemetry events").into_response();
            }
        };

        for event in telemetry_events.drain(..) {
            event_bus.send(event).await.expect("infallible");
        }

        (StatusCode::OK, "OK").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::DateTime;

    use crate::telemetry::events::{InitPhase, InitType, TelemetryRecord};

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_handle() {
        let event_body = Body::from(
            r#"[{"time":"2024-04-25T17:35:59.944Z","type":"platform.initStart","record":{"initializationType":"on-demand","phase":"init","runtimeVersion":"nodejs:20.v22","runtimeVersionArn":"arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72","functionName":"hello-world","functionVersion":"$LATEST"}}]"#,
        );
        let req = Request::builder()
            .method("POST")
            .uri("http://localhost:8080")
            .body(event_body)
            .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        // Create a new request with the body for testing
        let (parts, body) = req.into_parts();
        let req = Request::from_parts(parts, body);

        let response = TelemetryListener::handle(axum::extract::State(tx), req).await;

        // Check that the response is OK
        assert_eq!(response.status(), axum::http::StatusCode::OK);

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
