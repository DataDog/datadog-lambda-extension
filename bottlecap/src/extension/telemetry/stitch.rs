//! Reassembles telemetry payloads holding a log record that Lambda cut into pieces.
//!
//! A log record longer than about 256 KiB is cut mid-value into pieces, each an event of its
//! own. When a payload ends on a cut piece, the rest of it — plus the rest of the batch —
//! arrives in the next POST, under the next piece's envelope. Neither half parses on its own:
//!
//! ```text
//! POST 1  [{"time":"..929Z","type":"function","record":{"message":"iVBORw0KGgoAAA}]
//!         `------------- envelope --------------------'`--- cut here ---'`framing'
//!
//! POST 2  [{"time":"..930Z","type":"function","record":QICAgIfAhkiAAA"}},{..},{..runtimeDone..}]
//!         `------ the next piece's envelope ----------'`-- resumed --'`- rest of the batch -'
//! ```
//!
//! So the two are joined by dropping POST 1's framing and POST 2's envelope. Each piece carries
//! its own timestamp, so nothing pairs them but the joined bytes parsing.
//!
//! Pieces can also arrive next to each other in one POST. The seam between two such pieces is the
//! cut piece's closing `}` followed by the next piece's envelope, and it is dropped the same way.
//! A seam has the same bytes as the boundary between two whole events. It is told apart by the
//! record value before it, which was cut short and so does not parse alone.
//!
//! Recovering the batch matters beyond the log line itself. `platform.runtimeDone` lands in
//! the second half, and the on-demand loop waits for it before calling `/next` — so dropping
//! the batch holds the invocation open until Lambda times out the sandbox.

use serde::de::IgnoredAny;
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

/// The API writes the record's closing `}` and the array's `]` even after cutting the
/// record's value short, so a fragment ends with framing that belongs to neither half.
const FRAMING: &[u8] = b"}]";

/// Opens every event's envelope.
const EVENT_START: &[u8] = b"{\"time\":\"";

/// How a piece's envelope ends: only log lines are long enough to be split.
const PIECE_TYPES: &[&[u8]] = &[
    b"\"type\":\"function\",\"record\":",
    b"\"type\":\"extension\",\"record\":",
];

/// Room for an envelope's timestamp and type, bounding the search for its `record` key.
const MAX_ENVELOPE_BYTES: usize = 128;

/// Bounds the re-parsing of one record: a 4 MiB payload of 256 KiB pieces has 16 seams.
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
            let held_len = head.body.len();
            if let Some(resumed) = Fragment::resumed_bytes(body) {
                // A record can be cut more than once, so keep accumulating.
                match parse_or_hold(&mut slot, head.join(resumed)) {
                    Stitch::Discarded => {}
                    stitched => return stitched,
                }
            }

            debug!(
                "TELEMETRY API | Dropping {held_len} held bytes, the next payload does not continue it"
            );
        }

        parse_or_hold(&mut slot, body.to_vec())
    }
}

/// Parses `body` once its seams are dropped, or holds it if it ends on a cut piece.
fn parse_or_hold(slot: &mut Option<Fragment>, mut body: Vec<u8>) -> Stitch {
    match serde_json::from_slice(&body) {
        Ok(events) => return Stitch::Complete(events),
        // Valid JSON our types don't accept arrived whole; no seam explains it.
        Err(e) if e.classify() == Category::Data => return Stitch::Discarded,
        Err(_) => {}
    }

    if drop_seams(&mut body) && body.len() <= MAX_FRAGMENT_BYTES {
        *slot = Some(Fragment {
            body,
            received: Instant::now(),
        });
        return Stitch::Pending;
    }
    serde_json::from_slice(&body).map_or(Stitch::Discarded, Stitch::Complete)
}

/// The leading half of a split payload.
struct Fragment {
    body: Vec<u8>,
    received: Instant,
}

impl Fragment {
    /// The bytes that resume this fragment, if `body` is its continuation.
    ///
    /// A continuation opens with the next piece's envelope, so its `record` key is the first
    /// one in the payload — anything the customer nested sits inside the value that follows.
    fn resumed_bytes(body: &[u8]) -> Option<&[u8]> {
        let event = body.strip_prefix(b"[")?;
        // Only a log piece continues a fragment; any other type is a whole event.
        let (len, Some(_)) = envelope(event)? else {
            return None;
        };
        event.get(len..)
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

/// Joins the pieces of each cut record in `body`, and returns whether it ends on a cut piece to
/// hold for the rest.
///
/// A cut record continues in the next event of its type. A whole event of another type in
/// between, such as an extension log line, is moved ahead of it.
fn drop_seams(body: &mut Vec<u8>) -> bool {
    let Some(events) = split_events(body) else {
        return false;
    };

    let mut joined = Vec::with_capacity(body.len());
    // A record cut short, and its pieces joined so far.
    let mut cut: Option<(Event, Vec<u8>)> = None;
    let mut seam_count = 0;
    for event in events {
        match &mut cut {
            None => match parse_record(event.value) {
                Record::Whole => push_event(&mut joined, event.envelope, event.value),
                Record::Cut if event.kind.is_some() => cut = Some((event, event.value.to_vec())),
                _ => return false,
            },
            Some((record, _)) if event.kind != record.kind => {
                if !matches!(parse_record(event.value), Record::Whole) {
                    return false;
                }
                push_event(&mut joined, event.envelope, event.value);
            }
            Some((record, value)) => {
                if seam_count == MAX_SEAMS {
                    return false;
                }
                seam_count += 1;
                value.extend_from_slice(event.value);
                match parse_record(value) {
                    Record::Whole => {
                        push_event(&mut joined, record.envelope, value);
                        cut = None;
                    }
                    Record::Cut => {}
                    Record::Broken => return false,
                }
            }
        }
    }

    let ends_cut = cut.is_some();
    if let Some((record, value)) = cut {
        push_event(&mut joined, record.envelope, &value);
    }
    joined.push(b']');
    *body = joined;
    ends_cut
}

/// A payload's event: its envelope through `"record":`, its record value, and its type if it is
/// a log piece.
#[derive(Clone, Copy)]
struct Event<'a> {
    envelope: &'a [u8],
    value: &'a [u8],
    kind: Option<&'static [u8]>,
}

/// Splits a payload into its events.
fn split_events(body: &[u8]) -> Option<Vec<Event<'_>>> {
    let mut rest = body.strip_prefix(b"[")?.strip_suffix(FRAMING)?;
    let mut events = Vec::new();
    loop {
        let (len, kind) = envelope(rest)?;
        let (envelope, tail) = rest.split_at(len);
        let Some((value, next)) = split_at_next_event(tail) else {
            events.push(Event {
                envelope,
                value: tail,
                kind,
            });
            return Some(events);
        };
        events.push(Event {
            envelope,
            value,
            kind,
        });
        rest = next;
    }
}

/// Splits `bytes` at the first `},` that ends an event, which is one followed by an envelope.
fn split_at_next_event(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    (0..bytes.len()).find_map(|i| {
        let next = bytes[i..].strip_prefix(b"},")?;
        envelope(next).map(|_| (&bytes[..i], next))
    })
}

/// Appends an event to a payload being rebuilt, opening the array before the first one.
fn push_event(payload: &mut Vec<u8>, envelope: &[u8], value: &[u8]) {
    payload.push(if payload.is_empty() { b'[' } else { b',' });
    payload.extend_from_slice(envelope);
    payload.extend_from_slice(value);
    payload.push(b'}');
}

/// A record value taken on its own.
enum Record {
    Whole,
    /// Valid JSON until it runs out, so the next piece can complete it.
    Cut,
    Broken,
}

fn parse_record(value: &[u8]) -> Record {
    match serde_json::from_slice::<IgnoredAny>(value) {
        Ok(_) => Record::Whole,
        // A number cut after its `-`, `.` or `e` is reported as invalid at its last byte rather
        // than as running out.
        Err(e) if e.is_eof() || offset(value, &e) + 1 >= value.len() => Record::Cut,
        Err(_) => Record::Broken,
    }
}

/// Length of an event's envelope, through `"record":`, and its type if it is a log piece.
fn envelope(event: &[u8]) -> Option<(usize, Option<&'static [u8]>)> {
    let window = &event[..event.len().min(MAX_ENVELOPE_BYTES)];
    let fields = window.strip_prefix(EVENT_START)?;
    let len = find(fields, RECORD_KEY)? + RECORD_KEY.len();

    // The envelope holds only the timestamp and type; a brace means the match ran into a value.
    let fields = &fields[..len];
    if fields.iter().any(|b| matches!(b, b'{' | b'}')) {
        return None;
    }
    let kind = PIECE_TYPES
        .iter()
        .copied()
        .find(|tail| fields.ends_with(tail));
    Some((EVENT_START.len() + len, kind))
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

    /// The continuation: the next piece's envelope, the resumed bytes, then the rest of the batch.
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

        // The cut record is the last of several.
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

    /// A record logged as JSON whose structure, not just its text, holds arrays of objects.
    const STRUCTURED_RECORD: &str =
        r#"{"items":[{"id":1,"tags":["a","b"]},{"id":23,"ok":true,"v":null}],"n":-5}"#;

    const RUNTIME_DONE: &str = r#"{"time":"2026-09-03T14:29:52.950Z","type":"platform.runtimeDone","record":{"requestId":"abc123","status":"success"}}"#;

    /// A piece of a record as Lambda delivers it: raw, under an envelope of its own.
    fn piece(millis: &str, bytes: &str) -> String {
        format!(r#"{{"time":"2026-09-03T14:29:52.{millis}Z","type":"function","record":{bytes}}}"#)
    }

    fn assert_whole_record(record: &str, events: &[TelemetryEvent]) {
        let record = serde_json::from_str(record).expect("valid record");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].record, TelemetryRecord::Function(record));
        assert!(matches!(
            events[1].record,
            TelemetryRecord::PlatformRuntimeDone { .. }
        ));
    }

    /// Each record above, cut in two at every char boundary.
    fn cuts() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
        [RECORD, STRUCTURED_RECORD].into_iter().flat_map(|record| {
            (1..record.len())
                .filter(move |&cut| record.is_char_boundary(cut))
                .map(move |cut| {
                    let (first, rest) = record.split_at(cut);
                    (record, first, rest)
                })
        })
    }

    #[test]
    fn joins_pieces_within_a_payload_at_every_cut() {
        // Another stream's event, such as the extension's own log line, can land between pieces.
        let extension =
            r#"{"time":"2026-09-03T14:29:52.931Z","type":"extension","record":"flushing"},"#;
        for (record, first, rest) in cuts() {
            for between in ["", extension] {
                let body = format!(
                    "[{},{between}{},{RUNTIME_DONE}]",
                    piece("929", first),
                    piece("930", rest)
                );
                // Known gap: a cut inside a number in an array of objects can leave the body
                // valid JSON, so it never reaches the stitcher.
                if serde_json::from_str::<Vec<TelemetryEvent>>(&body).is_ok() {
                    continue;
                }

                let Stitch::Complete(mut events) = stitch(&FragmentBuffer::default(), &body) else {
                    panic!("pieces cut after {first:?} were not joined");
                };
                if !between.is_empty() {
                    assert!(matches!(
                        events.remove(0).record,
                        TelemetryRecord::Extension(_)
                    ));
                }
                assert_whole_record(record, &events);
            }
        }
    }

    #[test]
    fn joins_pieces_across_payloads_at_every_cut() {
        for (record, first, rest) in cuts() {
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
            assert_whole_record(record, &events);
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
        assert_whole_record(RECORD, &events);
    }

    #[test]
    fn does_not_mistake_a_broken_record_for_pieces() {
        let body = r#"[{"time":"2026-09-03T14:29:52.929Z","type":"function","record":{"message":"ok"}},{"time":"2026-09-03T14:29:52.930Z","type":"function","record":{"message":"bad\q"}}]"#;
        assert!(matches!(
            stitch(&FragmentBuffer::default(), body),
            Stitch::Discarded
        ));
    }

    /// `platform.runtimeDone` after a cut piece is moved ahead of it rather than glued into it.
    #[test]
    fn keeps_runtime_done_whole_after_a_cut_piece() {
        let fragments = FragmentBuffer::default();

        let head = format!("[{},{RUNTIME_DONE}]", piece("929", r#"{"message":"AA"#));
        assert!(matches!(stitch(&fragments, &head), Stitch::Pending));

        let tail = format!("[{}]", piece("930", r#"BB"}"#));
        let events = completed(stitch(&fragments, &tail));
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].record,
            TelemetryRecord::PlatformRuntimeDone { .. }
        ));
        assert_eq!(
            events[1].record,
            TelemetryRecord::Function(serde_json::json!({"message": "AABB"}))
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

        // Another cut record after a whole event, which doesn't parse joined on, so the held
        // fragment goes and this one takes its place.
        let other = r#"[{"time":"2026-09-03T14:30:11.000Z","type":"extension","record":"ready"},{"time":"2026-09-03T14:30:11.001Z","type":"function","record":{"message":"CCCC}]"#;
        assert!(matches!(stitch(&fragments, other), Stitch::Pending));

        // Proof the first fragment is gone: the continuation now resumes this one.
        let events = completed(stitch(&fragments, TAIL));
        assert_eq!(
            events[1].record,
            TelemetryRecord::Function(serde_json::json!({"message": "CCCCBBBB"}))
        );
    }

    #[test]
    fn recovers_a_payload_that_does_not_continue_the_held_fragment() {
        let fragments = FragmentBuffer::default();

        assert!(matches!(stitch(&fragments, HEAD), Stitch::Pending));

        // Whole once its own seam is dropped, so it must not go down with the held fragment.
        let (first, rest) = RECORD.split_at(20);
        let body = format!(
            "[{},{},{RUNTIME_DONE}]",
            piece("931", first),
            piece("932", rest)
        );
        assert_whole_record(RECORD, &completed(stitch(&fragments, &body)));
    }

    #[test]
    fn does_not_resume_a_fragment_with_a_whole_event() {
        let fragments = FragmentBuffer::default();

        assert!(matches!(stitch(&fragments, HEAD), Stitch::Pending));

        // Only a log piece continues one, so this record isn't glued onto the held message.
        let body = r#"[{"time":"2026-09-03T14:29:52.930Z","type":"platform.runtimeDone","record":BBBB"}}]"#;
        assert!(matches!(stitch(&fragments, body), Stitch::Discarded));
    }
}
