//! Invocation payloads: the 38 trigger events in `tests/payloads/`, plus large generated
//! SQS and DynamoDB events.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::fixtures::traces::BASE_NS;

fn payloads_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("payloads")
}

/// Reads `tests/payloads/<name>.json`.
pub fn payload(name: &str) -> String {
    let path = payloads_dir().join(format!("{name}.json"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Names of every file in `tests/payloads/`, sorted, without the `.json` suffix.
pub fn payload_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(payloads_dir())
        .expect("list tests/payloads")
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            name.strip_suffix(".json").map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

/// Every payload, parsed, with its name.
pub fn all_payloads() -> Vec<(String, Value)> {
    payload_names()
        .into_iter()
        .map(|name| {
            let value: Value = serde_json::from_str(&payload(&name))
                .unwrap_or_else(|e| panic!("parse payload {name}: {e}"));
            (name, value)
        })
        .collect()
}

/// An SQS batch of `records` messages, each with a JSON array body of exactly `body_bytes`
/// bytes (at least 2) and a `_datadog` message attribute carrying trace context. Sent at
/// [`BASE_NS`].
pub fn sqs_event(records: usize, body_bytes: usize) -> Value {
    let mut body = String::from("[");
    for i in 0.. {
        let item = format!("{{\"k{i}\":\"v{i}xxxxxxxxxxxxxxxxxxxxxx\"}},");
        // Keep one byte for the closing `]`.
        if body.len() + item.len() >= body_bytes {
            break;
        }
        body.push_str(&item);
    }
    // Pad with spaces before the `]` to reach the exact size.
    let body = format!(
        "{:<width$}]",
        body.trim_end_matches(','),
        width = body_bytes.saturating_sub(1)
    );
    let sent_ms = BASE_NS / 1_000_000;
    let recs: Vec<Value> = (0..records)
        .map(|i| {
            json!({
                "messageId": format!("059f36b4-87a3-44ab-83d2-66197{i:07}"),
                "receiptHandle": "AQEBwJnKyrHigUMZj6rYigCgxlaS3SLy0a...",
                "body": body,
                "attributes": {
                    "ApproximateReceiveCount": "1",
                    "SentTimestamp": sent_ms.to_string(),
                    "SenderId": "AIDAIENQZJOLO23YVJ4VO",
                    "ApproximateFirstReceiveTimestamp": (sent_ms + 2).to_string()
                },
                "messageAttributes": {
                    "_datadog": {
                        "stringValue": "{\"x-datadog-trace-id\":\"2684756524522091840\",\"x-datadog-parent-id\":\"7431398482019833808\",\"x-datadog-sampling-priority\":\"1\"}",
                        "stringListValues": [],
                        "binaryListValues": [],
                        "dataType": "String"
                    }
                },
                "md5OfBody": "e4e68fb7bd0e697a0ae8f1bb342846b3",
                "eventSource": "aws:sqs",
                "eventSourceARN": "arn:aws:sqs:us-east-1:123456789012:my-queue",
                "awsRegion": "us-east-1"
            })
        })
        .collect();
    json!({ "Records": recs })
}

/// The reference SQS payload: 10 records of 22 KB, about 250 KB serialized.
pub fn sqs_220kb() -> Value {
    sqs_event(10, 22_000)
}

/// A DynamoDB stream batch of `records` INSERT records with `attrs` attributes each, created
/// at [`BASE_NS`].
pub fn dynamodb_event(records: usize, attrs: usize) -> Value {
    let recs: Vec<Value> = (0..records)
        .map(|i| {
            let mut img = serde_json::Map::new();
            for a in 0..attrs {
                img.insert(format!("attr{a}"), json!({"S": format!("value-{i}-{a}")}));
            }
            json!({
                "eventID": format!("{i}"),
                "eventName": "INSERT",
                "eventVersion": "1.1",
                "eventSource": "aws:dynamodb",
                "awsRegion": "us-east-1",
                "dynamodb": {
                    "ApproximateCreationDateTime": BASE_NS / 1_000_000_000,
                    "Keys": {"Id": {"N": "101"}},
                    "NewImage": Value::Object(img),
                    "SequenceNumber": format!("{i}00000000000000000"),
                    "SizeBytes": 26,
                    "StreamViewType": "NEW_AND_OLD_IMAGES"
                },
                "eventSourceARN": "arn:aws:dynamodb:us-east-1:123456789012:table/ExampleTableWithStream/stream/2015-06-27T00:48:05.899"
            })
        })
        .collect();
    json!({ "Records": recs })
}

/// The reference DynamoDB payload: 100 records of 30 attributes, about 100 KB serialized.
pub fn dynamodb_100_records() -> Value {
    dynamodb_event(100, 30)
}

/// Headers of the `GET /runtime/invocation/next` response that LWA intercepts. The deadline is
/// 60 s from now, so a runtime client that honors it does not time out at once.
pub fn lwa_next_headers(request_id: &str) -> Vec<(&'static str, String)> {
    let deadline_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis()
        + 60_000;
    vec![
        ("content-type", "application/json".to_string()),
        ("lambda-runtime-aws-request-id", request_id.to_string()),
        ("lambda-runtime-deadline-ms", deadline_ms.to_string()),
        (
            "lambda-runtime-invoked-function-arn",
            crate::fixtures::telemetry::FUNCTION_ARN.to_string(),
        ),
        (
            "lambda-runtime-trace-id",
            "Root=1-67d448e8-3ae320be0e2cdf2f53ccdaba;Lineage=1:73f724a8:0".to_string(),
        ),
    ]
}
