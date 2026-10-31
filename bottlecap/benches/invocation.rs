//! Per-invocation payload work: trigger identification, payload parse and clones, and the
//! LWA parse.
//!
//! Review items: 5.10, 5.20.

mod common;
#[path = "../tests/fixtures/mod.rs"]
mod fixtures;

use std::borrow::Cow;
use std::collections::HashMap;
use std::hint::black_box;

use bottlecap::lifecycle::invocation::triggers::IdentifiedTrigger;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use serde::Deserialize;
use serde_json::{Value, json};

/// Payloads in `tests/payloads/` that are not trigger events: `eventbridge_span` is the expected
/// inferred span of a unit test.
const UNKNOWN_PAYLOADS: &[&str] = &["eventbridge_span"];

fn len_u64(bytes: &[u8]) -> u64 {
    bytes.len().try_into().expect("payload length fits in u64")
}

fn triggers(c: &mut Criterion) {
    let mut payloads = fixtures::events::all_payloads();
    payloads.push(("sqs_220kb".to_string(), fixtures::events::sqs_220kb()));
    payloads.push((
        "dynamodb_100_records".to_string(),
        fixtures::events::dynamodb_100_records(),
    ));

    let mut g = c.benchmark_group("invocation");
    for (name, value) in &payloads {
        let expect_unknown = UNKNOWN_PAYLOADS.contains(&name.as_str());
        assert_eq!(
            IdentifiedTrigger::from_value(value).is_unknown(),
            expect_unknown,
            "{name}: unexpected trigger identification"
        );
        let bytes = serde_json::to_vec(value).expect("serialize payload");
        g.throughput(Throughput::Bytes(len_u64(&bytes)));
        // Review 5.10: production calls this up to 3 times per invocation, each with a full
        // `Value` clone (`IdentifiedTrigger::from_value`): two call sites in `span_inferrer.rs`
        // and `InvocationProcessor::record_dsm_consume_from_payload`.
        g.bench_function(format!("trigger/from_value/{name}"), |b| {
            b.iter_with_large_drop(|| IdentifiedTrigger::from_value(black_box(value)));
        });
    }
    g.finish();
}

/// The fields the SQS trigger reads, borrowed from the input where no unescape is needed.
#[derive(Deserialize)]
struct SqsPartial<'a> {
    #[serde(rename = "Records", borrow)]
    records: Vec<SqsRecordPartial<'a>>,
}

#[derive(Deserialize)]
struct SqsRecordPartial<'a> {
    #[serde(rename = "eventSource", borrow)]
    event_source: Option<Cow<'a, str>>,
    #[serde(rename = "eventSourceARN", borrow)]
    event_source_arn: Option<Cow<'a, str>>,
    #[serde(rename = "messageAttributes", borrow)]
    message_attributes: Option<HashMap<Cow<'a, str>, MsgAttrPartial<'a>>>,
}

#[derive(Deserialize)]
struct MsgAttrPartial<'a> {
    #[serde(rename = "stringValue", borrow)]
    string_value: Option<Cow<'a, str>>,
}

fn sqs_datadog_contexts(bytes: &[u8]) -> usize {
    let parsed: SqsPartial = serde_json::from_slice(bytes).expect("typed sqs parse");
    parsed
        .records
        .iter()
        .filter(|r| r.event_source.as_deref() == Some("aws:sqs") && r.event_source_arn.is_some())
        .filter_map(|r| r.message_attributes.as_ref())
        .filter_map(|m| m.get("_datadog"))
        .filter_map(|a| a.string_value.as_deref())
        .count()
}

fn large_payloads() -> [(&'static str, Vec<u8>); 2] {
    [
        (
            "sqs_220kb",
            serde_json::to_vec(&fixtures::events::sqs_220kb()).expect("serialize sqs"),
        ),
        (
            "dynamodb_100",
            serde_json::to_vec(&fixtures::events::dynamodb_100_records())
                .expect("serialize dynamodb"),
        ),
    ]
}

fn payload(c: &mut Criterion) {
    let payloads = large_payloads();
    let mut g = c.benchmark_group("invocation");
    for (name, bytes) in &payloads {
        let value: Value = serde_json::from_slice(bytes).expect("parse payload");
        g.throughput(Throughput::Bytes(len_u64(bytes)));
        // The start-invocation handler in src/lifecycle/listener.rs
        g.bench_function(format!("payload/parse/{name}"), |b| {
            b.iter_with_large_drop(|| serde_json::from_slice::<Value>(bytes).expect("parse"));
        });
        // `IdentifiedTrigger::from_value`
        g.bench_function(format!("payload/clone/{name}"), |b| {
            b.iter_with_large_drop(|| value.clone());
        });
        // Today's tracer path: one parse, then one clone per `from_value` call. Drops are timed.
        g.bench_function(format!("payload/parse_plus_3_clones/{name}"), |b| {
            b.iter(|| {
                let v = serde_json::from_slice::<Value>(bytes).expect("parse");
                let clones = [v.clone(), v.clone(), v.clone()];
                black_box((&v, &clones));
            });
        });
    }

    // The comparison target for the SQS case: a borrowed typed parse of the fields used.
    let (name, bytes) = &payloads[0];
    assert_eq!(
        sqs_datadog_contexts(bytes),
        10,
        "one _datadog context per record"
    );
    g.throughput(Throughput::Bytes(len_u64(bytes)));
    g.bench_function(format!("payload/typed_partial_parse/{name}"), |b| {
        b.iter(|| sqs_datadog_contexts(bytes));
    });
    g.finish();
}

/// Mirrors `process_invocation_next` in src/lwa/mod.rs: parse, serialize again, then
/// parse a clone of the new body.
fn lwa_parse(body_bytes: &[u8]) -> (Value, Vec<u8>, Value) {
    let inner_payload = serde_json::from_slice::<Value>(body_bytes).unwrap_or_else(|_| json!({}));
    let body = serde_json::to_vec(&inner_payload).unwrap_or_default();
    let payload_value =
        serde_json::from_slice::<Value>(&body.clone()).unwrap_or_else(|_| json!({}));
    (inner_payload, body, payload_value)
}

fn lwa(c: &mut Criterion) {
    let [sqs, dynamodb] = large_payloads();
    let api_gateway = (
        "api_gateway_http_event",
        fixtures::events::payload("api_gateway_http_event").into_bytes(),
    );
    let mut g = c.benchmark_group("invocation");
    for (name, bytes) in [sqs, dynamodb, api_gateway] {
        assert!(lwa_parse(&bytes).2.is_object(), "{name}: parse failed");
        g.throughput(Throughput::Bytes(len_u64(&bytes)));
        g.bench_function(format!("lwa/parse/{name}"), |b| {
            b.iter_with_large_drop(|| lwa_parse(&bytes));
        });
    }
    g.finish();
}

criterion_group! {
    name = benches;
    config = common::criterion();
    targets = triggers, payload, lwa
}
criterion_main!(benches);
