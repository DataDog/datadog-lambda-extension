use crate::{
    event_bus,
    extension::telemetry::{
        events::TelemetryEvent,
        stitch::{FragmentBuffer, Stitch},
    },
    http::{extract_request_body, handler_not_found},
};

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Body ceiling, replacing axum's 2 MiB default so a full-size POST — up to
/// `2 * maxBytes + metadataBytes` — isn't rejected before the handler sees it.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct TelemetryListener {
    host: [u8; 4],
    port: u16,
    cancel_token: CancellationToken,
    logs_tx: Sender<TelemetryEvent>,
    event_bus_tx: Sender<event_bus::Event>,
}

#[derive(Clone)]
struct HandlerState {
    logs_tx: Sender<TelemetryEvent>,
    fragments: FragmentBuffer,
}

impl TelemetryListener {
    #[must_use]
    pub fn new(
        host: [u8; 4],
        port: u16,
        logs_tx: Sender<TelemetryEvent>,
        event_bus_tx: Sender<event_bus::Event>,
    ) -> Self {
        let cancel_token = CancellationToken::new();
        Self {
            host,
            port,
            cancel_token,
            logs_tx,
            event_bus_tx,
        }
    }

    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    pub fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        let socket = SocketAddr::from((self.host, self.port));
        let router = self.make_router();

        let cancel_token_clone = self.cancel_token();
        let event_bus_tx = self.event_bus_tx.clone();
        tokio::spawn(async move {
            let listener = TcpListener::bind(&socket)
                .await
                .expect("Failed to bind socket");
            debug!("TELEMETRY API | Starting listener on {}", socket);
            axum::serve(listener, router)
                .with_graceful_shutdown(Self::graceful_shutdown(cancel_token_clone, event_bus_tx))
                .await
                .expect("Failed to start telemetry listener");
        });

        Ok(())
    }

    fn make_router(&self) -> Router {
        let state = HandlerState {
            logs_tx: self.logs_tx.clone(),
            fragments: FragmentBuffer::default(),
        };

        Router::new()
            .route("/", post(Self::handle))
            .fallback(handler_not_found)
            .with_state(state)
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
    }

    async fn graceful_shutdown(
        cancel_token: CancellationToken,
        event_bus_tx: Sender<event_bus::Event>,
    ) {
        cancel_token.cancelled().await;
        debug!("TELEMETRY API | Shutdown signal received, sending tombstone event");

        // Send tombstone event to signal shutdown
        if let Err(e) = event_bus_tx.send(event_bus::Event::Tombstone).await {
            debug!("TELEMETRY API |Failed to send tombstone event: {:?}", e);
        }

        debug!("TELEMETRY API | Shutting down");
    }

    async fn handle(State(state): State<HandlerState>, request: Request) -> Response {
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

        let mut telemetry_events: Vec<TelemetryEvent> = match serde_json::from_slice(&body) {
            Ok(events) => events,
            // The Telemetry API splits an oversized record across two POSTs, and neither half
            // parses alone. See `stitch`.
            Err(e) => match state.fragments.stitch(&body, &e) {
                Stitch::Complete(events) => {
                    debug!(
                        "TELEMETRY API | Reassembled a split payload, recovered {} events",
                        events.len()
                    );
                    events
                }
                Stitch::Pending => {
                    return (StatusCode::OK, "Holding split telemetry payload").into_response();
                }
                Stitch::Discarded => {
                    // If we can't parse the event, we will receive it again in a new batch
                    // causing an infinite loop and resource contention.
                    // Instead, log it and move on.
                    // This will result in a dropped payload, but may be from
                    // events we haven't added support for yet
                    let body = String::from_utf8_lossy(&body);
                    debug!("Failed to parse telemetry events `{body}`, failed with: {e}");
                    return (StatusCode::OK, "Failed to parse telemetry events").into_response();
                }
            },
        };

        for event in telemetry_events.drain(..) {
            state.logs_tx.send(event).await.expect("infallible");
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

    use crate::extension::telemetry::events::{InitPhase, InitType, TelemetryRecord};
    use crate::extension::telemetry::stitch::fixtures::{HEAD, TAIL};

    fn state(logs_tx: Sender<TelemetryEvent>) -> HandlerState {
        HandlerState {
            logs_tx,
            fragments: FragmentBuffer::default(),
        }
    }

    fn post(body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("http://localhost:8080")
            .body(Body::from(body.to_string()))
            .expect("failed to build request")
    }

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

        let response = TelemetryListener::handle(axum::extract::State(state(tx)), req).await;

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

    /// A split payload is held on the first POST and forwarded whole on the second.
    #[tokio::test]
    async fn test_handle_split_payload() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let state = state(tx);

        let response =
            TelemetryListener::handle(axum::extract::State(state.clone()), post(HEAD)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(rx.try_recv().is_err(), "held fragment must not be emitted");

        let response = TelemetryListener::handle(axum::extract::State(state), post(TAIL)).await;
        assert_eq!(response.status(), StatusCode::OK);

        assert!(matches!(
            rx.try_recv().expect("function record").record,
            TelemetryRecord::Function(_)
        ));
        assert!(matches!(
            rx.try_recv().expect("runtimeDone record").record,
            TelemetryRecord::PlatformRuntimeDone { .. }
        ));
    }
}
