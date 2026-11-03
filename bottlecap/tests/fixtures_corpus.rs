//! Checks the fixture generators, and writes the corpus to disk on request.
//!
//! Write the corpus with:
//! `cargo test --test fixtures_corpus write_fixture_corpus -- --ignored`

mod fixtures;

use bottlecap::extension::telemetry::events::TelemetryEvent;
use bottlecap::extension::telemetry::stitch::{FragmentBuffer, Stitch};
use bottlecap::lifecycle::invocation::triggers::IdentifiedTrigger;
use fixtures::{dogstatsd, events, logs, telemetry, traces};

#[test]
fn telemetry_batches_parse() {
    let events: Vec<TelemetryEvent> =
        serde_json::from_slice(&telemetry::platform_batch(telemetry::REQUEST_ID)).unwrap();
    assert_eq!(events.len(), 6);
    for kind in telemetry::LogKind::ALL {
        let events: Vec<TelemetryEvent> =
            serde_json::from_slice(&telemetry::log_batch(kind, 10)).unwrap();
        assert_eq!(events.len(), 10, "{}", kind.name());
    }
    // The largest shape, as `write_corpus` writes it, fits the 1 MiB `maxBytes`.
    assert!(telemetry::log_batch(telemetry::LogKind::JsonString, 600).len() <= 1 << 20);
    let events: Vec<TelemetryEvent> =
        serde_json::from_slice(&telemetry::invocation_batch(telemetry::LogKind::Text, 5)).unwrap();
    assert_eq!(events.len(), 8);
}

#[test]
fn split_record_stitches_back_together() {
    let posts = telemetry::split_record_posts(1 << 20, 256 << 10, 3);
    assert_eq!(posts.len(), 2);
    let fragments = FragmentBuffer::default();
    assert!(serde_json::from_slice::<Vec<TelemetryEvent>>(&posts[0]).is_err());
    assert!(matches!(fragments.stitch(&posts[0]), Stitch::Pending));
    match fragments.stitch(&posts[1]) {
        Stitch::Complete(events) => assert_eq!(events.len(), 2),
        other => panic!("expected a complete stitch, got {other:?}"),
    }

    let single = telemetry::split_record_single_post(1 << 20, 256 << 10);
    assert!(serde_json::from_slice::<Vec<TelemetryEvent>>(&single).is_err());
    match FragmentBuffer::default().stitch(&single) {
        Stitch::Complete(events) => assert_eq!(events.len(), 2),
        other => panic!("expected a complete stitch, got {other:?}"),
    }
}

#[test]
fn trace_payload_round_trips() {
    let traces = traces::trace_2000_spans();
    assert_eq!(traces.iter().map(Vec::len).sum::<usize>(), 2000);
    let body = traces::v04_msgpack(&traces);
    let decoded: Vec<Vec<libdd_trace_protobuf::pb::Span>> = rmp_serde::from_slice(&body).unwrap();
    assert_eq!(decoded.len(), 100);
    assert_eq!(
        body,
        traces::v04_2000_spans(),
        "encoding is not deterministic"
    );
}

#[test]
fn log_lines_have_the_expected_shape() {
    assert!(logs::text_line(0).len() >= 200);
    assert!(logs::json_object(0).is_object());
    assert_eq!(logs::text_line_of_len(1, 2000).len(), 2000);
    assert_eq!(logs::OOM_LINES.len(), 7);
}

#[test]
fn dogstatsd_packets_split_by_lines() {
    let packets = dogstatsd::packets(dogstatsd::Kind::Count, 200, 20);
    assert_eq!(packets.len(), 10);
    assert_eq!(packets[0].lines().count(), 20);
    assert!(packets.iter().all(|p| p.len() < 8192));
}

#[test]
fn large_events_identify_as_their_trigger() {
    assert_eq!(events::all_payloads().len(), 38);
    let sqs = events::sqs_220kb();
    assert_eq!(sqs["Records"][0]["body"].as_str().unwrap().len(), 22_000);
    let sqs_len = serde_json::to_vec(&sqs).unwrap().len();
    assert!((200_000..260_000).contains(&sqs_len), "sqs {sqs_len} B");
    assert!(matches!(
        IdentifiedTrigger::from_value(&sqs),
        IdentifiedTrigger::SqsRecord(_)
    ));
    let ddb = events::dynamodb_100_records();
    let ddb_len = serde_json::to_vec(&ddb).unwrap().len();
    assert!((80_000..140_000).contains(&ddb_len), "dynamodb {ddb_len} B");
    assert!(matches!(
        IdentifiedTrigger::from_value(&ddb),
        IdentifiedTrigger::DynamoDbRecord(_)
    ));
}

#[test]
#[ignore = "writes bottlecap/tests/fixtures/generated/; run on demand"]
fn write_fixture_corpus() {
    let dir = fixtures::default_corpus_dir();
    let written = fixtures::write_corpus(&dir);
    for (path, len) in &written {
        println!("{len:>10}  {}", path.display());
    }
    assert!(written.len() > 20);
}
