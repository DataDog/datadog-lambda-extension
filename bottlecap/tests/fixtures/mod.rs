//! Shared fixture corpus for benchmarks, the fault-injection harness and the Lambda simulator.
//!
//! Everything is generated in-process, so nothing large is checked in. Use the generators
//! directly from a bench or a test:
//!
//! ```ignore
//! #[path = "../tests/fixtures/mod.rs"]
//! mod fixtures;
//! let body = fixtures::telemetry::log_batch(fixtures::telemetry::LogKind::Text, 1000);
//! ```
//!
//! To write the corpus to disk once (for tools that read files, such as a simulator written in
//! another language), run:
//!
//! ```text
//! cargo test --test fixtures_corpus write_fixture_corpus -- --ignored
//! ```
//!
//! That writes `bottlecap/tests/fixtures/generated/` (gitignored), or the directory named by
//! `BOTTLECAP_FIXTURES_DIR`. See `README.md` in this directory for the file list.
#![allow(dead_code, clippy::missing_panics_doc, clippy::must_use_candidate)]

pub mod dogstatsd;
pub mod events;
pub mod logs;
pub mod telemetry;
pub mod traces;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Default output directory for [`write_corpus`]: `bottlecap/tests/fixtures/generated`.
pub fn default_corpus_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BOTTLECAP_FIXTURES_DIR") {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("generated")
}

/// Writes every fixture to `dir` and returns the written files with their sizes.
pub fn write_corpus(dir: &Path) -> Vec<(PathBuf, usize)> {
    let mut written = Vec::new();
    let mut put = |rel: &str, bytes: &[u8]| {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().expect("fixture path has a parent"))
            .expect("create fixture directory");
        fs::write(&path, bytes).expect("write fixture");
        written.push((path, bytes.len()));
    };

    // Telemetry API POST bodies.
    put(
        "telemetry/platform_batch.json",
        &telemetry::platform_batch(telemetry::REQUEST_ID),
    );
    for (kind, name) in [
        (telemetry::LogKind::Text, "function_text_1000.json"),
        (
            telemetry::LogKind::JsonObject,
            "function_json_object_1000.json",
        ),
        (
            telemetry::LogKind::JsonString,
            "function_json_string_1000.json",
        ),
    ] {
        put(
            &format!("telemetry/{name}"),
            &telemetry::log_batch(kind, 1000),
        );
    }
    put(
        "telemetry/invocation_batch_text_200.json",
        &telemetry::invocation_batch(telemetry::LogKind::Text, 200),
    );
    for (i, post) in telemetry::split_record_posts(1 << 20, 256 << 10, 3)
        .iter()
        .enumerate()
    {
        put(&format!("telemetry/split_record_1mib_post_{i}.json"), post);
    }
    put(
        "telemetry/split_record_1mib_single_post.json",
        &telemetry::split_record_single_post(1 << 20, 256 << 10),
    );

    // Traces: one v0.4 msgpack body of 2000 spans.
    put("traces/v04_2000_spans.msgpack", &traces::v04_2000_spans());
    put(
        "traces/v04_100_spans.msgpack",
        &traces::v04_msgpack(&traces::make_traces(5, 20, true)),
    );

    // Raw log lines.
    let text: Vec<String> = (0..1000).map(logs::text_line).collect();
    put("logs/text_lines_1000.txt", text.join("\n").as_bytes());
    let json: Vec<String> = (0..1000).map(logs::json_line).collect();
    put("logs/json_lines_1000.jsonl", json.join("\n").as_bytes());
    put("logs/oom_lines.txt", logs::OOM_LINES.join("\n").as_bytes());

    // DogStatsD datagrams, one file per packet.
    for (i, packet) in dogstatsd::packets(dogstatsd::Kind::Count, 200, 20)
        .iter()
        .enumerate()
    {
        put(
            &format!("dogstatsd/count_packet_{i}.txt"),
            packet.as_bytes(),
        );
    }
    for (i, packet) in dogstatsd::packets(dogstatsd::Kind::Distribution, 200, 20)
        .iter()
        .enumerate()
    {
        put(
            &format!("dogstatsd/distribution_packet_{i}.txt"),
            packet.as_bytes(),
        );
    }
    put(
        "dogstatsd/enrich_tags.txt",
        dogstatsd::ENRICH_TAGS.as_bytes(),
    );

    // Large invocation payloads, plus a copy of the checked-in trigger events so a tool that
    // reads this directory finds every payload in one place.
    put(
        "events/sqs_220kb.json",
        &serde_json::to_vec(&events::sqs_220kb()).expect("serialize sqs event"),
    );
    put(
        "events/dynamodb_100_records.json",
        &serde_json::to_vec(&events::dynamodb_100_records()).expect("serialize dynamodb event"),
    );
    for name in events::payload_names() {
        put(
            &format!("events/payloads/{name}.json"),
            events::payload(&name).as_bytes(),
        );
    }

    let mut manifest = String::new();
    for (path, len) in &written {
        let rel = path.strip_prefix(dir).unwrap_or(path);
        manifest.push_str(&format!("{len:>10}  {}\n", rel.display()));
    }
    let manifest_path = dir.join("MANIFEST.txt");
    let mut f = fs::File::create(&manifest_path).expect("create manifest");
    f.write_all(manifest.as_bytes()).expect("write manifest");
    written.push((manifest_path, manifest.len()));
    written
}
