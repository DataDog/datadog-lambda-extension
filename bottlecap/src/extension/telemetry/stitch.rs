//! Reassembles telemetry payloads that the Telemetry API split across two POSTs.
//!
//! A record larger than the subscription's `maxBytes` is cut mid-value. The rest of it — plus
//! the rest of the batch — arrives in the next POST, which repeats the cut record's envelope
//! ahead of the resumed bytes. Neither half parses on its own:
//!
//! ```text
//! POST 1  [{"time":"..929Z","type":"function","record":{"message":"iVBORw0KGgoAAA}]
//!         `------------- envelope --------------------'`--- cut here ---'`framing'
//!
//! POST 2  [{"time":"..929Z","type":"function","record":QICAgIfAhkiAAA"}},{..},{..runtimeDone..}]
//!         `------- the same envelope, repeated -------'`-- resumed --'`- rest of the batch -'
//! ```
//!
//! So the two are joined by dropping POST 1's framing and POST 2's repeated envelope. That
//! byte-identical envelope, which carries the cut record's timestamp, is the only thing
//! pairing them: the API sends no sequence number.
//!
//! Recovering the batch matters beyond the log line itself. `platform.runtimeDone` lands in
//! the second half, and the on-demand loop waits for it before calling `/next` — so dropping
//! the batch holds the invocation open until Lambda times out the sandbox.

use serde_json::error::Category;
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};
use tracing::debug;

use crate::extension::telemetry::events::TelemetryEvent;

/// Ceiling on a held fragment. Sized above the largest single POST — the API can send up to
/// `2 * maxBytes + metadataBytes`, so over 2 MiB at the 1 MiB `maxBytes` we subscribe with —
/// while still bounding the accumulation when one record is cut repeatedly.
const MAX_FRAGMENT_BYTES: usize = 4 * 1024 * 1024;

/// Fragments arrive back to back, so one held this long is waiting on a continuation that
/// never came.
const FRAGMENT_TTL: Duration = Duration::from_secs(1);

/// Precedes a record's value, so everything up to and including it is the envelope.
const RECORD_KEY: &[u8] = b"\"record\":";

/// Opens a record.
const RECORD_START: &[u8] = b"{\"time\":";

/// The API writes the record's closing `}` and the array's `]` even after cutting the
/// record's value short, so a fragment ends with framing that belongs to neither half.
const FRAMING: &[u8] = b"}]";

/// What came of pairing an unparseable body with a held fragment.
#[derive(Debug)]
pub(crate) enum Stitch {
    /// The body completed a split payload.
    Complete(Vec<TelemetryEvent>),
    /// The body opens a split payload, and is held for its continuation.
    Pending,
    /// The body is not part of a split payload.
    Discarded,
}

/// Holds the leading half of a split payload until its continuation arrives.
#[derive(Clone, Default)]
pub(crate) struct FragmentBuffer {
    held: Arc<Mutex<Option<Fragment>>>,
}

impl FragmentBuffer {
    /// Joins `body` onto the held fragment, or holds `body` if it opens a split payload.
    ///
    /// `error` is the failure `body` produced on its own; it is how a payload cut mid-record
    /// is told apart from one we simply can't interpret.
    pub(crate) fn stitch(&self, body: &[u8], error: &serde_json::Error) -> Stitch {
        let mut slot = self.held.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(stale) = slot.take_if(|held| held.received.elapsed() > FRAGMENT_TTL) {
            debug!(
                "TELEMETRY API | Dropping {} held bytes, no continuation arrived",
                stale.body.len()
            );
        }

        if let Some(head) = slot.take() {
            if let Some(resumed) = head.resumed_bytes(body) {
                let joined = head.join(resumed);
                return match serde_json::from_slice(&joined) {
                    Ok(events) => Stitch::Complete(events),
                    // A record can be cut more than once, so keep accumulating.
                    Err(e) => hold_or_discard(&mut slot, joined, &e),
                };
            }

            debug!(
                "TELEMETRY API | Dropping {} held bytes, the next payload does not continue it",
                head.body.len()
            );
        }

        hold_or_discard(&mut slot, body.to_vec(), error)
    }
}

/// Holds `body` for its continuation, or reports that nothing can be recovered from it.
fn hold_or_discard(
    slot: &mut Option<Fragment>,
    body: Vec<u8>,
    error: &serde_json::Error,
) -> Stitch {
    *slot = Fragment::from_cut_payload(body, error);
    if slot.is_some() {
        Stitch::Pending
    } else {
        Stitch::Discarded
    }
}

/// The leading half of a split payload.
struct Fragment {
    body: Vec<u8>,
    /// The envelope the continuation repeats before the resumed bytes.
    repeated_envelope: Vec<u8>,
    received: Instant,
}

impl Fragment {
    /// A fragment, if `body` is the leading half of a split payload: an array that ran out of
    /// input inside its last record's value. Any other parse failure won't be fixed by
    /// joining, and holding such a body would poison the next stitch.
    fn from_cut_payload(body: Vec<u8>, error: &serde_json::Error) -> Option<Self> {
        if error.classify() != Category::Eof
            || body.len() > MAX_FRAGMENT_BYTES
            || body.first() != Some(&b'[')
        {
            return None;
        }

        // Rebuild the cut record's envelope as the continuation will send it: `[` then the
        // record's keys, up to the value that got cut. The cut record is the last one here.
        let value_start = rfind(&body, RECORD_KEY)? + RECORD_KEY.len();
        let record_start = rfind(body.get(..value_start)?, RECORD_START)?;

        let mut repeated_envelope = vec![b'['];
        repeated_envelope.extend_from_slice(body.get(record_start..value_start)?);

        Some(Self {
            body,
            repeated_envelope,
            received: Instant::now(),
        })
    }

    /// The bytes that resume this fragment, if `body` is its continuation.
    fn resumed_bytes<'a>(&self, body: &'a [u8]) -> Option<&'a [u8]> {
        body.strip_prefix(self.repeated_envelope.as_slice())
    }

    /// Joins the resumed bytes on, dropping the framing: left in place it would land inside
    /// the resumed value, where it parses but corrupts the record.
    fn join(mut self, resumed: &[u8]) -> Vec<u8> {
        if self.body.ends_with(FRAMING) {
            self.body.truncate(self.body.len() - FRAMING.len());
        }
        self.body.extend_from_slice(resumed);
        self.body
    }
}

/// Offset of the last occurrence of `needle` in `haystack`.
fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// The two halves of a real split payload, trimmed to the bytes that matter.
#[cfg(test)]
pub(crate) mod fixtures {
    /// A `function` record cut inside its `message` value, plus the framing.
    pub(crate) const HEAD: &str =
        r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":{"message":"AAAA}]"#;

    /// The continuation: the same envelope, the resumed bytes, then the rest of the batch.
    pub(crate) const TAIL: &str = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":BBBB"}},{"time":"2026-09-03T14:29:52.930Z","type":"platform.runtimeDone","record":{"requestId":"abc123","status":"success","metrics":{"durationMs":18.074,"producedBytes":329814}}}]"#;
}

#[cfg(test)]
mod tests {
    use super::fixtures::{HEAD, TAIL};
    use super::*;
    use crate::extension::telemetry::events::{RuntimeDoneMetrics, Status, TelemetryRecord};

    /// Mirrors the handler: a body only reaches the buffer once it has failed to parse.
    fn stitch(fragments: &FragmentBuffer, body: &str) -> Stitch {
        let error = serde_json::from_slice::<Vec<TelemetryEvent>>(body.as_bytes())
            .expect_err("fixture must not parse on its own");
        fragments.stitch(body.as_bytes(), &error)
    }

    /// The events of a stitch that should have completed, reporting what came back if it did not.
    fn completed(stitch: Stitch) -> Vec<TelemetryEvent> {
        match stitch {
            Stitch::Complete(events) => events,
            other => panic!("expected the continuation to complete the payload, got {other:?}"),
        }
    }

    #[test]
    fn joins_a_split_payload() {
        let fragments = FragmentBuffer::default();

        assert!(matches!(stitch(&fragments, HEAD), Stitch::Pending));

        let events = completed(stitch(&fragments, TAIL));

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
    fn joins_a_payload_cut_after_whole_records() {
        let fragments = FragmentBuffer::default();

        // The cut record is the last of several, so the envelope the continuation repeats is
        // in the middle of the fragment.
        let head = format!(
            r#"[{{"time":"2026-09-03T14:29:52.900Z","type":"extension","record":"ready"}},{}"#,
            HEAD.trim_start_matches('[')
        );
        assert!(matches!(stitch(&fragments, &head), Stitch::Pending));

        let events = completed(stitch(&fragments, TAIL));
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1].record,
            TelemetryRecord::Function(serde_json::json!({"message": "AAAABBBB"}))
        );
    }

    #[test]
    fn does_not_hold_a_payload_that_arrived_whole() {
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
    fn drops_a_fragment_the_next_payload_does_not_continue() {
        let fragments = FragmentBuffer::default();

        assert!(matches!(stitch(&fragments, HEAD), Stitch::Pending));

        // A different record's envelope, so the held fragment goes and this one takes its
        // place.
        let other =
            r#"[{"time":"2026-09-03T14:30:11.001Z","type":"function","record":{"message":"CCCC}]"#;
        assert!(matches!(stitch(&fragments, other), Stitch::Pending));

        // Proof the first fragment is gone: its own continuation no longer joins.
        assert!(matches!(stitch(&fragments, TAIL), Stitch::Discarded));
    }
}
