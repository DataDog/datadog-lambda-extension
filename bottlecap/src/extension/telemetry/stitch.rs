//! Reassembles telemetry payloads that the Telemetry API split across two POSTs.
//!
//! A log record longer than about 256 KiB is cut mid-value into pieces, each an event of its
//! own. When a payload ends on a cut piece, the rest of it — plus the rest of the batch —
//! arrives in the next POST, under the next piece's envelope. Neither half parses on its own:
//!
//! ```text
//! POST 1  [{"time":"..929Z","type":"function","record":{"message":"iVBORw0KGgoAAA}]
//!         `------------- envelope --------------------'`--- cut here ---'`framing'
//!
//! POST 2  [{"time":"..929Z","type":"function","record":QICAgIfAhkiAAA"}},{..},{..runtimeDone..}]
//!         `------ the next piece's envelope ----------'`-- resumed --'`- rest of the batch -'
//! ```
//!
//! So the two are joined by dropping POST 1's framing and POST 2's envelope. Each piece carries
//! its own timestamp, so nothing pairs them but the joined bytes parsing.
//!
//! Pieces also sit side by side within one POST, where the seam between them — the cut piece's
//! closing `}` and the next piece's envelope — is dropped the same way. Serde fails at, or just
//! after, a seam, which tells it apart from the boundary between two whole events.
//!
//! Recovering the batch matters beyond the log line itself. `platform.runtimeDone` lands in
//! the second half, and the on-demand loop waits for it before calling `/next` — so dropping
//! the batch holds the invocation open until Lambda times out the sandbox.

use serde_json::error::Category;
use std::{
    ops::Range,
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

/// The API writes the record's closing `}` and the array's `]` even after cutting the
/// record's value short, so a fragment ends with framing that belongs to neither half.
const FRAMING: &[u8] = b"}]";

/// Opens a seam between two pieces: the cut piece's closing `}`, then the next one's envelope.
const SEAM: &[u8] = b"},{\"time\":\"";

/// How a piece's envelope ends: only log lines are long enough to be split.
const PIECE_TYPES: &[&[u8]] = &[
    b"\"type\":\"function\",\"record\":",
    b"\"type\":\"extension\",\"record\":",
];

/// How far past a seam's start serde can fail: a cut inside a string swallows `},{"`, and
/// the string closes on the `"` before `time`.
const SEAM_REACH: usize = 4;

/// Bounds the re-parsing of one payload: a 4 MiB payload of 256 KiB pieces has 16 seams.
const MAX_SEAMS: usize = 64;

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
    pub(crate) fn stitch(&self, body: &[u8]) -> Stitch {
        let mut slot = self.held.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(stale) = slot.take_if(|held| held.received.elapsed() > FRAGMENT_TTL) {
            debug!(
                "TELEMETRY API | Dropping {} held bytes, no continuation arrived",
                stale.body.len()
            );
        }

        if let Some(head) = slot.take() {
            if let Some(resumed) = Fragment::resumed_bytes(body) {
                let mut joined = head.join(resumed);
                return match unsplit(&mut joined) {
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

        let mut body = body.to_vec();
        match unsplit(&mut body) {
            Ok(events) => Stitch::Complete(events),
            Err(e) => hold_or_discard(&mut slot, body, &e),
        }
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
    received: Instant,
}

impl Fragment {
    /// A fragment, if `body` is the leading half of a split payload: an array whose last piece
    /// was cut short, running out of input or into the framing. Any other parse failure won't
    /// be fixed by joining, and holding such a body would poison the next stitch.
    fn from_cut_payload(body: Vec<u8>, error: &serde_json::Error) -> Option<Self> {
        let cut_short = match error.classify() {
            Category::Eof => true,
            // Cut after a `\` or a whole value, the piece runs into the framing instead.
            Category::Syntax => offset(&body, error) + FRAMING.len() >= body.len(),
            _ => false,
        };
        if !cut_short || body.len() > MAX_FRAGMENT_BYTES || body.first() != Some(&b'[') {
            return None;
        }

        Some(Self {
            body,
            received: Instant::now(),
        })
    }

    /// The bytes that resume this fragment, if `body` is its continuation.
    ///
    /// A continuation opens with the next piece's envelope, so its `record` key is the first
    /// one in the payload — anything the customer nested sits inside the value that follows.
    fn resumed_bytes(body: &[u8]) -> Option<&[u8]> {
        let payload = body.strip_prefix(b"[")?;
        let value_start = find(payload, RECORD_KEY)? + RECORD_KEY.len();
        payload.get(value_start..)
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

/// Offset of the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Parses `body`, dropping the seams between pieces one at a time where serde fails on them.
fn unsplit(body: &mut Vec<u8>) -> Result<Vec<TelemetryEvent>, serde_json::Error> {
    let mut seams = 0;
    loop {
        let error = match serde_json::from_slice(body) {
            Ok(events) => return Ok(events),
            Err(e) => e,
        };
        if seams == MAX_SEAMS || error.classify() == Category::Data {
            return Err(error);
        }
        match seam_at(body, offset(body, &error)) {
            Some(seam) => body.drain(seam),
            None => return Err(error),
        };
        seams += 1;
    }
}

/// The seam serde failed on at byte `at`, if there is one.
fn seam_at(body: &[u8], at: usize) -> Option<Range<usize>> {
    let start = (at.saturating_sub(SEAM_REACH)..=at)
        .rev()
        .find(|&i| body.get(i..).is_some_and(|rest| rest.starts_with(SEAM)))?;

    // The envelope holds only the timestamp and a log type; a brace means the match ran into a
    // value, and any other type is a whole event — say `platform.runtimeDone` — not a piece.
    let envelope = body.get(start + SEAM.len()..)?;
    let len = find(envelope, RECORD_KEY)? + RECORD_KEY.len();
    let envelope = &envelope[..len];
    if envelope.iter().any(|b| matches!(b, b'{' | b'}'))
        || !PIECE_TYPES.iter().any(|tail| envelope.ends_with(tail))
    {
        return None;
    }

    Some(start..start + SEAM.len() + len)
}

/// The byte serde's one-based line and column point at.
fn offset(body: &[u8], error: &serde_json::Error) -> usize {
    let line_start = match error.line() {
        0 | 1 => 0,
        line => body
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'\n')
            .nth(line - 2)
            .map_or(0, |(i, _)| i + 1),
    };
    line_start + error.column().saturating_sub(1)
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
        serde_json::from_slice::<Vec<TelemetryEvent>>(body.as_bytes())
            .expect_err("fixture must not parse on its own");
        fragments.stitch(body.as_bytes())
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

    /// A structured record can nest a `record` key of its own, which must not be mistaken for
    /// the envelope's.
    #[test]
    fn joins_a_structured_record_that_nests_a_record_key() {
        let fragments = FragmentBuffer::default();

        let head = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":{"level":"INFO","message":{"record":"AAAA}]"#;
        let tail = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":BBBB"}}},{"time":"2026-09-03T14:29:52.930Z","type":"platform.runtimeDone","record":{"requestId":"abc123","status":"success","metrics":{"durationMs":18.074,"producedBytes":329814}}}]"#;

        assert!(matches!(stitch(&fragments, head), Stitch::Pending));

        let events = completed(stitch(&fragments, tail));
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].record,
            TelemetryRecord::Function(
                serde_json::json!({"level": "INFO", "message": {"record": "AAAABBBB"}})
            )
        );
    }

    /// The same key inside a string is escaped, so it never looked like the envelope's.
    #[test]
    fn joins_a_message_whose_text_looks_like_a_record_key() {
        let fragments = FragmentBuffer::default();

        let head = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":{"message":"{\"record\":\"AAAA}]"#;
        let tail = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":BBBB\"}"}},{"time":"2026-09-03T14:29:52.930Z","type":"platform.runtimeDone","record":{"requestId":"abc123","status":"success","metrics":{"durationMs":18.074,"producedBytes":329814}}}]"#;

        assert!(matches!(stitch(&fragments, head), Stitch::Pending));

        let events = completed(stitch(&fragments, tail));
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].record,
            TelemetryRecord::Function(serde_json::json!({"message": r#"{"record":"AAAABBBB"}"#}))
        );
    }

    /// A record logged as JSON, with the escapes, multi-byte text, nesting, numbers and literals a
    /// cut can land in.
    const RECORD: &str = r#"{"level":"info","msg":"{\"a\":[1,true,null],\"b\":\"café caf\u00e9 \\\\ x\"}","n":-1.5e3}"#;

    const RUNTIME_DONE: &str = r#"{"time":"2026-09-03T14:29:52.950Z","type":"platform.runtimeDone","record":{"requestId":"abc123","status":"success"}}"#;

    /// A piece of `RECORD` as Lambda delivers it: raw, under an envelope of its own.
    fn piece(millis: &str, bytes: &str) -> String {
        format!(r#"{{"time":"2026-09-03T14:29:52.{millis}Z","type":"function","record":{bytes}}}"#)
    }

    fn assert_whole_record(events: &[TelemetryEvent]) {
        let record = serde_json::from_str(RECORD).expect("valid record");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].record, TelemetryRecord::Function(record));
        assert!(matches!(
            events[1].record,
            TelemetryRecord::PlatformRuntimeDone { .. }
        ));
    }

    #[test]
    fn joins_pieces_within_a_payload_at_every_cut() {
        for cut in (1..RECORD.len()).filter(|&cut| RECORD.is_char_boundary(cut)) {
            let (first, rest) = RECORD.split_at(cut);
            let body = format!(
                "[{},{},{RUNTIME_DONE}]",
                piece("929", first),
                piece("930", rest)
            );

            let Stitch::Complete(events) = stitch(&FragmentBuffer::default(), &body) else {
                panic!("pieces cut after {first:?} were not joined");
            };
            assert_whole_record(&events);
        }
    }

    #[test]
    fn joins_pieces_across_payloads_at_every_cut() {
        for cut in (1..RECORD.len()).filter(|&cut| RECORD.is_char_boundary(cut)) {
            let (first, rest) = RECORD.split_at(cut);
            let fragments = FragmentBuffer::default();

            let head = format!("[{}]", piece("929", first));
            assert!(
                matches!(stitch(&fragments, &head), Stitch::Pending),
                "payload ending after {first:?} was not held"
            );

            let tail = format!("[{},{RUNTIME_DONE}]", piece("930", rest));
            let Stitch::Complete(events) = stitch(&fragments, &tail) else {
                panic!("payloads cut after {first:?} were not joined");
            };
            assert_whole_record(&events);
        }
    }

    /// The shape seen in production: a record in several pieces, the payload ending between
    /// two of them.
    #[test]
    fn joins_a_record_in_pieces_across_payloads() {
        let fragments = FragmentBuffer::default();

        let (a, rest) = RECORD.split_at(20);
        let (b, rest) = rest.split_at(20);
        let (c, d) = rest.split_at(20);

        let head = format!(
            r#"[{{"time":"2026-09-03T14:29:52.900Z","type":"extension","record":"ready"}},{},{}]"#,
            piece("929", a),
            piece("930", b)
        );
        assert!(matches!(stitch(&fragments, &head), Stitch::Pending));

        let tail = format!("[{},{},{RUNTIME_DONE}]", piece("931", c), piece("932", d));
        let mut events = completed(stitch(&fragments, &tail));

        assert_eq!(events.len(), 3);
        assert!(matches!(
            events.remove(0).record,
            TelemetryRecord::Extension(_)
        ));
        assert_whole_record(&events);
    }

    #[test]
    fn does_not_mistake_a_broken_record_for_pieces() {
        let body = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":{"message":"ok"}},{"time":"2026-09-03T14:29:52.930Z","type":"function","record":{"message":"bad\q"}}]"#;
        assert!(matches!(
            stitch(&FragmentBuffer::default(), body),
            Stitch::Discarded
        ));

        // Fails where `platform.runtimeDone` begins, which a log piece's seam never does.
        let body = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":},{"time":"2026-09-03T14:29:52.930Z","type":"platform.runtimeDone","record":{"requestId":"abc123","status":"success"}}]"#;
        assert!(matches!(
            stitch(&FragmentBuffer::default(), body),
            Stitch::Discarded
        ));
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

        // Another cut record, which doesn't parse joined on, so the held fragment goes with it.
        let other =
            r#"[{"time":"2026-09-03T14:30:11.001Z","type":"function","record":{"message":"CCCC}]"#;
        assert!(matches!(stitch(&fragments, other), Stitch::Discarded));

        // Proof the first fragment is gone: its own continuation no longer joins.
        assert!(matches!(stitch(&fragments, TAIL), Stitch::Discarded));
    }
}
