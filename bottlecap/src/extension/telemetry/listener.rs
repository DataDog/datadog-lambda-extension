use crate::{
    event_bus,
    extension::telemetry::events::TelemetryEvent,
    http::{extract_request_body, handler_not_found},
};

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::error::Category;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Ceiling on a held fragment. The Telemetry API can POST up to
/// `2 * maxBytes + metadataBytes`, so this fits a full-size head at the 1 MiB `maxBytes` we
/// subscribe with.
const MAX_FRAGMENT_BYTES: usize = 2 * 1024 * 1024;

/// Body ceiling, replacing axum's 2 MiB default so a full-size POST isn't rejected before
/// the handler sees it.
const MAX_BODY_BYTES: usize = 2 * MAX_FRAGMENT_BYTES;

/// A continuation POST follows its head immediately, so anything older belongs to a payload
/// whose continuation never arrived.
const FRAGMENT_TTL: Duration = Duration::from_secs(1);

const RECORD_KEY: &[u8] = b"\"record\":";
const RECORD_START: &[u8] = b"{\"time\":";

/// The Telemetry API writes a record's framing even after cutting its value short, so a head
/// fragment ends with the envelope's `}` and the array's `]`.
const APPENDED_CLOSERS: &[u8] = b"}]";

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

/// Outcome of trying to pair an unparseable body with a held fragment.
#[derive(Debug)]
enum Stitch {
    /// The body completed a split payload.
    Complete(Vec<TelemetryEvent>),
    /// The body opens a split payload; it is held for its continuation.
    Pending,
    /// The body is not part of a split payload, or its continuation never came.
    Discarded,
}

/// Reassembles telemetry payloads that the Telemetry API split across consecutive POSTs.
///
/// A record larger than the subscription's `maxBytes` is cut mid-value and resumed in the
/// next POST, which repeats the cut record's envelope (`[{"time":..,"type":..,"record":`)
/// ahead of the resumed bytes. Neither half parses on its own, and the second half is where
/// the rest of the batch lands — including the `platform.runtimeDone` that the on-demand
/// loop waits on before calling `/next`.
#[derive(Clone, Default)]
struct FragmentBuffer {
    held: Arc<Mutex<Option<Fragment>>>,
}

impl FragmentBuffer {
    /// Joins `body` onto a held fragment, or holds it if it opens a split payload.
    ///
    /// Assumes the continuation is the next POST: the Telemetry API sends fragments back to
    /// back and gives no sequence number to correlate on. The repeated envelope, which
    /// carries the cut record's timestamp, is what pairs the two halves.
    fn stitch(&self, body: &[u8], error: &serde_json::Error) -> Stitch {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(Fragment {
            body: mut joined,
            continuation_prefix,
            received,
        }) = held.take()
        {
            if received.elapsed() > FRAGMENT_TTL {
                debug!(
                    "TELEMETRY API | Dropping {} held bytes, no continuation arrived",
                    joined.len()
                );
            } else if let Some(tail) = body.strip_prefix(continuation_prefix.as_slice()) {
                // Left in place, the head's closers land inside the resumed value, where
                // they parse but corrupt the record.
                if joined.ends_with(APPENDED_CLOSERS) {
                    joined.truncate(joined.len() - APPENDED_CLOSERS.len());
                }
                joined.extend_from_slice(tail);

                return match serde_json::from_slice(&joined) {
                    Ok(events) => Stitch::Complete(events),
                    // A record can be cut more than once, so keep going.
                    Err(e) => Self::hold(&mut held, joined, &e),
                };
            } else {
                debug!(
                    "TELEMETRY API | Dropping {} held bytes, next payload does not continue it",
                    joined.len()
                );
            }
        }

        Self::hold(&mut held, body.to_vec(), error)
    }

    fn hold(held: &mut Option<Fragment>, body: Vec<u8>, error: &serde_json::Error) -> Stitch {
        *held = Fragment::opening(body, error);
        if held.is_some() {
            Stitch::Pending
        } else {
            Stitch::Discarded
        }
    }
}

struct Fragment {
    body: Vec<u8>,
    /// What the continuation POST repeats before the resumed bytes: `[` followed by the
    /// keys of the cut record up to and including `"record":`.
    continuation_prefix: Vec<u8>,
    received: Instant,
}

impl Fragment {
    /// Holds `body` only if it opens a split payload: an array that ran out of input inside
    /// the value of its last record. Any other parse failure won't be fixed by joining, and
    /// holding it would poison the next stitch.
    fn opening(body: Vec<u8>, error: &serde_json::Error) -> Option<Self> {
        if error.classify() != Category::Eof
            || body.len() > MAX_FRAGMENT_BYTES
            || body.first() != Some(&b'[')
        {
            return None;
        }

        // The cut record is the last one in the body.
        let key_end = rfind(&body, RECORD_KEY)? + RECORD_KEY.len();
        let record_start = rfind(body.get(..key_end)?, RECORD_START)?;

        let mut continuation_prefix = vec![b'['];
        continuation_prefix.extend_from_slice(body.get(record_start..key_end)?);

        Some(Self {
            body,
            continuation_prefix,
            received: Instant::now(),
        })
    }
}

/// Byte offset of the last occurrence of `needle` in `haystack`.
fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::DateTime;

    use crate::extension::telemetry::events::{
        InitPhase, InitType, RuntimeDoneMetrics, Status, TelemetryRecord,
    };

    /// Leading half of a split payload: a `function` record cut inside its `message` value,
    /// with the framing the Telemetry API writes anyway.
    const HEAD: &str =
        r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":{"message":"AAAA}]"#;

    /// Its continuation, which repeats the cut record's envelope before the resumed bytes
    /// and carries the rest of the batch.
    const TAIL: &str = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":BBBB"}},{"time":"2026-09-03T14:29:52.930Z","type":"platform.runtimeDone","record":{"requestId":"abc123","status":"success","metrics":{"durationMs":18.074,"producedBytes":329814}}}]"#;

    /// Mirrors the handler: a body only reaches the buffer once it has failed to parse.
    fn stitch(fragments: &FragmentBuffer, body: &str) -> Stitch {
        let error = serde_json::from_slice::<Vec<TelemetryEvent>>(body.as_bytes())
            .expect_err("fixture must not parse on its own");
        fragments.stitch(body.as_bytes(), &error)
    }

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

    #[test]
    fn test_stitch_split_payload() {
        let fragments = FragmentBuffer::default();

        assert!(matches!(stitch(&fragments, HEAD), Stitch::Pending));

        let Stitch::Complete(events) = stitch(&fragments, TAIL) else {
            panic!("expected the continuation to complete the payload");
        };

        assert_eq!(events.len(), 2);
        // `AAAA}]BBBB` would mean the head's framing was left in the resumed value.
        assert_eq!(
            events[0].record,
            TelemetryRecord::Function(serde_json::json!({"message": "AAAABBBB"}))
        );
        assert_eq!(
            events[1].record,
            TelemetryRecord::PlatformRuntimeDone {
                request_id: "abc123".to_string(),
                status: Status::Success,
                error_type: None,
                metrics: Some(RuntimeDoneMetrics {
                    duration_ms: 18.074,
                    produced_bytes: Some(329_814),
                }),
            }
        );

        // The pair is consumed, so a following payload starts from nothing.
        assert!(matches!(stitch(&fragments, TAIL), Stitch::Discarded));
    }

    #[test]
    fn test_complete_payload_is_not_held() {
        let fragments = FragmentBuffer::default();

        // Parses as JSON, so it failed for a reason joining won't fix. Holding it would
        // poison the next stitch.
        let unsupported =
            r#"[{"time":"2026-09-03T14:29:52.929Z","type":"platform.brandNew","record":{}}]"#;
        assert!(matches!(stitch(&fragments, unsupported), Stitch::Discarded));

        assert!(matches!(stitch(&fragments, HEAD), Stitch::Pending));
        assert!(matches!(stitch(&fragments, TAIL), Stitch::Complete(_)));
    }

    #[test]
    fn test_head_dropped_when_next_payload_is_unrelated() {
        let fragments = FragmentBuffer::default();

        assert!(matches!(stitch(&fragments, HEAD), Stitch::Pending));

        // A different record's envelope: not this head's continuation, so the head goes and
        // this one is held in its place.
        let other =
            r#"[{"time":"2026-09-03T14:30:11.001Z","type":"function","record":{"message":"CCCC}]"#;
        assert!(matches!(stitch(&fragments, other), Stitch::Pending));

        // Proof the original head is gone: its continuation no longer stitches.
        assert!(matches!(stitch(&fragments, TAIL), Stitch::Discarded));
    }

    #[test]
    fn test_stitch_head_holding_complete_records() {
        let fragments = FragmentBuffer::default();

        // The cut record is the last of several, so the envelope the continuation repeats is
        // in the middle of the head.
        let head = format!(
            r#"[{{"time":"2026-09-03T14:29:52.900Z","type":"extension","record":"ready"}},{}"#,
            HEAD.trim_start_matches('[')
        );
        assert!(matches!(stitch(&fragments, &head), Stitch::Pending));

        let Stitch::Complete(events) = stitch(&fragments, TAIL) else {
            panic!("expected the continuation to complete the payload");
        };
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1].record,
            TelemetryRecord::Function(serde_json::json!({"message": "AAAABBBB"}))
        );
    }

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
}
