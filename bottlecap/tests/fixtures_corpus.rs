//! Checks the fixture generators, and writes the corpus to disk on request.
//!
//! Write the corpus with:
//! `cargo test --test fixtures_corpus write_fixture_corpus -- --ignored`

mod fixtures;

use std::collections::HashMap;

use bottlecap::extension::telemetry::events::TelemetryEvent;
use bottlecap::extension::telemetry::stitch::{FragmentBuffer, Stitch};
use bottlecap::lifecycle::invocation::triggers::IdentifiedTrigger;
use fixtures::invocation::{
    ACCOUNT_ID, ENV, FUNCTION_ARN, FUNCTION_NAME, REGION, REQUEST_ID, SERVICE, VERSION, XRAY_ROOT,
};
use fixtures::{dogstatsd, events, logs, telemetry, traces};
use serde_json::Value;

#[test]
fn telemetry_batches_parse() {
    let events: Vec<TelemetryEvent> =
        serde_json::from_slice(&telemetry::platform_batch(REQUEST_ID)).unwrap();
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

/// Telemetry, logs, traces, Runtime API headers and tags describe the same invocation.
#[test]
fn fixtures_share_one_invocation() {
    let ns = |t: &Value| {
        chrono::DateTime::parse_from_rfc3339(t.as_str().unwrap())
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap()
    };
    let xray_header = format!("Root={XRAY_ROOT};");
    assert_eq!(
        FUNCTION_ARN,
        format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:{FUNCTION_NAME}")
    );

    let batch: Vec<Value> = serde_json::from_slice(&telemetry::invocation_batch(
        telemetry::LogKind::JsonObject,
        600,
    ))
    .unwrap();
    let (start, lines, done) = (&batch[0], &batch[1..601], &batch[601]);
    assert_eq!(start["record"]["requestId"], REQUEST_ID);
    assert_eq!(done["record"]["requestId"], REQUEST_ID);
    let tracing = start["record"]["tracing"]["value"].as_str().unwrap();
    assert!(tracing.starts_with(&xray_header));
    let window = ns(&start["time"])..=ns(&done["time"]);
    for (i, line) in lines.iter().enumerate() {
        let (time, record) = (ns(&line["time"]), &line["record"]);
        assert!(window.contains(&time), "log {i}");
        assert_eq!(ns(&record["timestamp"]), time, "log {i}");
        assert_eq!(record["requestId"], REQUEST_ID);
        assert_eq!(record["function_name"], FUNCTION_NAME);
        assert_eq!(record["xray_trace_id"], XRAY_ROOT);
        let text = logs::text_line(i);
        let mut fields = text.split('\t');
        assert_eq!(ns(&fields.next().unwrap().into()), time, "text log {i}");
        assert_eq!(fields.next(), Some(REQUEST_ID));
    }

    for trace in traces::trace_2000_spans() {
        let root = &trace[0];
        assert_eq!(root.resource, FUNCTION_NAME);
        assert_eq!(root.meta["request_id"], REQUEST_ID);
        let root_span = root.start..=root.start + root.duration;
        assert!(window.contains(root_span.start()) && window.contains(root_span.end()));
        for span in &trace {
            assert!(
                root_span.contains(&span.start)
                    && root_span.contains(&(span.start + span.duration))
            );
            assert_eq!(span.service, SERVICE);
            assert_eq!(span.meta["env"], ENV);
            assert_eq!(span.meta["version"], VERSION);
        }
    }

    let headers: HashMap<_, _> = events::lwa_next_headers(REQUEST_ID).into_iter().collect();
    assert_eq!(headers["lambda-runtime-aws-request-id"], REQUEST_ID);
    assert_eq!(headers["lambda-runtime-invoked-function-arn"], FUNCTION_ARN);
    assert!(headers["lambda-runtime-trace-id"].starts_with(&xray_header));

    let tags = dogstatsd::enrich_tags();
    for tag in [
        format!("function_arn:{FUNCTION_ARN}"),
        format!("functionname:{FUNCTION_NAME}"),
        format!("service:{SERVICE}"),
    ] {
        assert!(tags.split(',').any(|t| t == tag), "{tag}");
    }
    assert!(dogstatsd::DIST_8_TAGS.contains(&format!(
        "env:{ENV},service:{SERVICE},version:{VERSION},region:{REGION}"
    )));
}

#[test]
fn write_corpus_removes_only_files_in_its_dir() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("corpus");
    let outside = root.path().join("outside.txt");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&outside, "keep").unwrap();
    for entry in ["../outside.txt".to_string(), outside.display().to_string()] {
        std::fs::write(dir.join("MANIFEST.txt"), format!("4  {entry}\n")).unwrap();
        assert!(std::panic::catch_unwind(|| fixtures::write_corpus(&dir)).is_err());
        assert!(outside.exists(), "{entry}");
    }
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
