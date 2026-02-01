use crate::{
    extension::telemetry::events::TelemetryEvent,
    http::{extract_request_body, handler_not_found},
};

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::mpsc::Sender, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::debug;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct TelemetryListener {
    host: [u8; 4],
    port: u16,
    cancel_token: CancellationToken,
    logs_tx: Sender<TelemetryEvent>,
}

impl TelemetryListener {
    #[must_use]
    pub fn new(host: [u8; 4], port: u16, logs_tx: Sender<TelemetryEvent>) -> Self {
        let cancel_token = CancellationToken::new();
        Self {
            host,
            port,
            cancel_token,
            logs_tx,
        }
    }

    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }

    /// Starts the telemetry listener HTTP server.
    /// Returns a `JoinHandle` that completes when the server has fully shut down,
    /// ensuring all in-flight HTTP requests have been processed.
    #[must_use]
    pub fn start(&self) -> JoinHandle<()> {
        let socket = SocketAddr::from((self.host, self.port));
        let router = self.make_router();

        let cancel_token_clone = self.cancel_token();
        tokio::spawn(async move {
            let listener = TcpListener::bind(&socket)
                .await
                .expect("Failed to bind socket");
            debug!("TELEMETRY API | Starting listener on {}", socket);
            axum::serve(listener, router)
                .with_graceful_shutdown(Self::graceful_shutdown(cancel_token_clone))
                .await
                .expect("Failed to start telemetry listener");
            debug!("TELEMETRY API | HTTP server fully shut down");
        })
    }

    fn make_router(&self) -> Router {
        let logs_tx: Sender<TelemetryEvent> = self.logs_tx.clone();

        Router::new()
            .route("/", post(Self::handle))
            .fallback(handler_not_found)
            .with_state(logs_tx)
    }

    async fn graceful_shutdown(cancel_token: CancellationToken) {
        cancel_token.cancelled().await;
        debug!(
            "TELEMETRY API | Shutdown signal received, initiating graceful HTTP server shutdown"
        );
        // Note: Tombstone event is now sent by the main shutdown sequence
        // after all telemetry messages have been processed
    }

    async fn handle(State(logs_tx): State<Sender<TelemetryEvent>>, request: Request) -> Response {
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
                debug!("Failed to parse telemetry events `{body}`, failed with: {e}");
                return (StatusCode::OK, "Failed to parse telemetry events").into_response();
            }
        };

        for event in telemetry_events.drain(..) {
            logs_tx.send(event).await.expect("infallible");
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

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_start_returns_joinhandle() {
        // Test that start() returns a JoinHandle that can be awaited
        let (logs_tx, _logs_rx) = tokio::sync::mpsc::channel(10);
        let listener = TelemetryListener::new([127, 0, 0, 1], 0, logs_tx);

        let join_handle = listener.start();

        // Cancel immediately and await completion
        listener.cancel_token().cancel();

        // The JoinHandle should complete without hanging
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), join_handle).await;

        assert!(result.is_ok(), "JoinHandle should complete within timeout");
        assert!(result.unwrap().is_ok(), "Server task should not panic");
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_graceful_shutdown_completes() {
        // Test that graceful shutdown completes when cancel token is triggered
        let (logs_tx, _logs_rx) = tokio::sync::mpsc::channel(10);
        let listener = TelemetryListener::new([127, 0, 0, 1], 0, logs_tx);

        let cancel_token = listener.cancel_token();
        let join_handle = listener.start();

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Cancel and verify shutdown completes
        cancel_token.cancel();

        // Wait for shutdown to complete (should not hang)
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), join_handle).await;

        assert!(
            result.is_ok(),
            "Graceful shutdown should complete within timeout"
        );
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_no_tombstone_sent_on_shutdown() {
        // This test verifies that TelemetryListener no longer sends tombstone
        // The tombstone is now sent by main.rs after all messages are processed
        let (logs_tx, mut logs_rx) = tokio::sync::mpsc::channel(10);
        let listener = TelemetryListener::new([127, 0, 0, 1], 0, logs_tx);

        let cancel_token = listener.cancel_token();
        let join_handle = listener.start();

        // Give the server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Cancel the listener
        cancel_token.cancel();

        // Wait for shutdown
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), join_handle).await;

        // Verify no messages were sent to logs_rx (no tombstone)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            logs_rx.try_recv().is_err(),
            "No messages should be sent during shutdown (tombstone is sent by main.rs)"
        );
    }
}
