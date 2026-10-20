// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Contract tests for the logs pipeline.
//!
//! They pin what bottlecap sends to the logs intake today: the JSON of every log, request
//! URLs and headers, retry/redrive/drop behavior, and how many `warn!`/`error!` lines the
//! pipeline emits. Migrating to `datadog-logs-agent` must keep every expectation in this
//! file unchanged. Only HARNESS code (see the doc on `impl Harness`) may change with the
//! implementation.

#[path = "common/fake_logs_intake.rs"]
mod fake_logs_intake;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::Config;
use bottlecap::config::logs_additional_endpoints::LogsAdditionalEndpoint;
use bottlecap::config::processing_rule::{Kind, ProcessingRule};
use bottlecap::event_bus::EventBus;
use bottlecap::extension::telemetry::events::TelemetryEvent;
use bottlecap::logs::pipeline::{LogsPipeline, build_logs_pipeline};
use bottlecap::tags::provider::Provider;
use dogstatsd::api_key::ApiKeyFactory;
use fake_logs_intake::{FakeLogsIntake, RecordedRequest};
use serde_json::{Value, json};
use tracing::Level;
use tracing_subscriber::layer::SubscriberExt as _;

const ARN: &str = "arn:aws:lambda:us-east-1:123456789012:function:contract-test";
const REQUEST_ID: &str = "459921b5-681c-4a96-beb0-81e0aa586026";
const API_KEY: &str = "primary-api-key";
const SERVICE: &str = "contract-service"; // configured as "Contract-Service"; bottlecap lowercases it

const INIT_START: &str = r#"{"time":"2022-10-21T14:05:03.000Z","type":"platform.initStart","record":{"initializationType":"on-demand","phase":"init"}}"#;
const START: &str = r#"{"time":"2022-10-21T14:05:03.165Z","type":"platform.start","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","version":"$LATEST"}}"#;
const RUNTIME_DONE: &str = r#"{"time":"2022-10-21T14:05:05.764Z","type":"platform.runtimeDone","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","status":"success","metrics":{"durationMs":2599.0,"producedBytes":8}}}"#;
const REPORT: &str = r#"{"time":"2022-10-21T14:05:05.766Z","type":"platform.report","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","metrics":{"durationMs":2599.4,"billedDurationMs":2600,"memorySizeMB":128,"maxMemoryUsedMB":94,"initDurationMs":549.04},"status":"success"}}"#;
const MANAGED_INSTANCE_REPORT: &str = r#"{"time":"2022-10-21T14:05:05.766Z","type":"platform.report","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","status":"success","metrics":{"durationMs":1.148}}}"#;

const TS_INIT_START: i64 = 1_666_361_103_000;
const TS_START: i64 = 1_666_361_103_165;
const TS_FUNCTION: i64 = 1_666_361_104_000;
const TS_RUNTIME_DONE: i64 = 1_666_361_105_764;
const TS_REPORT: i64 = 1_666_361_105_766;

const START_LINE: &str = "START RequestId: 459921b5-681c-4a96-beb0-81e0aa586026 Version: $LATEST";
const INIT_START_LINE: &str = "INIT_START Runtime Version: ? Runtime Version ARN: ?";

fn function_log(record: impl Into<Value>) -> String {
    json!({"time": "2022-10-21T14:05:04.000Z", "type": "function", "record": record.into()})
        .to_string()
}

fn extension_log(record: &str) -> String {
    json!({"time": "2022-10-21T14:05:04.000Z", "type": "extension", "record": record}).to_string()
}

/// Config shared by all tests: logs go to `intake` uncompressed over HTTP/1.
fn base_config(intake: &FakeLogsIntake) -> Config {
    Config {
        http_protocol: Some("http1".to_string()),
        skip_ssl_validation: true,
        logs_config_logs_dd_url: intake.base_url(),
        logs_config_use_compression: false,
        logs_config_compression_level: 3,
        service: Some("Contract-Service".to_string()),
        flush_timeout: 5,
        ..Config::default()
    }
}

/// The logs from the one and only request `intake` received.
#[track_caller]
fn single_request_logs(intake: &FakeLogsIntake) -> Value {
    let requests = intake.requests();
    assert_eq!(requests.len(), 1, "expected exactly one request");
    requests[0].json()
}

/// One line per log entry (message length and the first 80 characters), for readable
/// failure messages when payloads are megabytes long.
fn summarize(logs: &Value) -> String {
    logs.as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let message = entry["message"]["message"].as_str().unwrap_or_default();
                    let preview: String = message.chars().take(80).collect();
                    format!("{} bytes: {preview}", message.len())
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "<not an array>".to_string())
}

/// Header names on a request, minus transport headers whose values depend on the
/// connection (`host`, `content-length`).
fn header_names(request: &RecordedRequest) -> BTreeSet<String> {
    request
        .headers
        .keys()
        .filter(|name| !matches!(name.as_str(), "host" | "content-length"))
        .cloned()
        .collect()
}

/// `entry` with its message cut to 80 characters, for readable failure output.
fn abbreviate(entry: &Value) -> Value {
    let mut entry = entry.clone();
    if let Some(message) = entry["message"]["message"].as_str() {
        let preview: String = message.chars().take(80).collect();
        let abbreviated = format!("{preview}... ({} bytes)", message.len());
        entry["message"]["message"] = Value::String(abbreviated);
    }
    entry
}

/// Asserts that `actual` holds every log in `expected` exactly once, in any order (logs are
/// matched by message, so messages must be unique).
#[track_caller]
fn assert_each_log_delivered_once(mut actual: Vec<Value>, mut expected: Vec<Value>) {
    let message = |entry: &Value| entry["message"]["message"].as_str().map(str::to_string);
    actual.sort_by_cached_key(message);
    expected.sort_by_cached_key(message);
    for (a, e) in actual.iter().zip(&expected) {
        assert!(
            a == e,
            "logs differ\n--- actual ---\n{:#}\n--- expected ---\n{:#}",
            abbreviate(a),
            abbreviate(e)
        );
    }
    assert_eq!(actual.len(), expected.len(), "number of logs delivered");
}

fn additional_endpoint(intake: &FakeLogsIntake, api_key: &str) -> LogsAdditionalEndpoint {
    LogsAdditionalEndpoint {
        api_key: api_key.to_string(),
        host: "127.0.0.1".to_string(),
        port: u32::from(intake.port()),
        is_reliable: true,
    }
}

struct Harness {
    pipeline: LogsPipeline,
    tags: String,
    _event_bus: EventBus, // keeps the event bus receiver alive so sends succeed
}

/// Everything in this `impl` except `send_invocation_start`, `log`, `log_with` and
/// `invocation_start_logs` is HARNESS: it wires the test to the current pipeline
/// implementation and may change when the implementation changes. Expected values elsewhere
/// in this file must not change. `tags` must come from the same tag provider the pipeline
/// uses.
impl Harness {
    fn new(config: Config) -> Self {
        Self::build(config, false, Arc::new(ApiKeyFactory::new(API_KEY)))
    }

    fn managed_instance(config: Config) -> Self {
        Self::build(config, true, Arc::new(ApiKeyFactory::new(API_KEY)))
    }

    /// An on-demand pipeline whose API key resolver never returns a key.
    fn with_unresolvable_api_key(config: Config) -> Self {
        let resolver: dogstatsd::api_key::ApiKeyResolverFn = Arc::new(
            || -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
                Box::pin(async { None })
            },
        );
        let factory = Arc::new(ApiKeyFactory::new_from_resolver(resolver, None));
        Self::build(config, false, factory)
    }

    fn build(
        config: Config,
        is_managed_instance_mode: bool,
        api_key_factory: Arc<ApiKeyFactory>,
    ) -> Self {
        let config = Arc::new(config);
        let tags_provider = Arc::new(Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), ARN.to_string())]),
        ));
        let tags = tags_provider.get_tags_string();
        let (event_bus, event_bus_tx) = EventBus::run();
        let client = bottlecap::http::get_client(&config);
        let pipeline = build_logs_pipeline(
            &config,
            api_key_factory,
            &tags_provider,
            event_bus_tx,
            is_managed_instance_mode,
            &client,
        );
        Self {
            pipeline,
            tags,
            _event_bus: event_bus,
        }
    }

    async fn send(&mut self, events: &[&str]) {
        for raw in events {
            let event: TelemetryEvent =
                serde_json::from_str(raw).expect("test event must be valid telemetry");
            self.pipeline
                .telemetry_tx
                .send(event)
                .await
                .expect("send telemetry event");
            self.pipeline.agent.sync_consume().await;
        }
    }

    async fn send_invocation_start(&mut self) {
        self.send(&[INIT_START, START]).await;
    }

    /// One normal flush, returning requests that should be redriven.
    async fn flush(&self) -> Vec<reqwest::RequestBuilder> {
        self.pipeline.flusher.flush(None).await
    }

    /// Mirrors `FlushingService::await_handles` — one redrive pass per failed
    /// request, results discarded. (Runs them one after another; the real service runs them
    /// concurrently, which makes no difference with one failed request. Blocking flushes
    /// discard failures entirely and never redrive.)
    async fn redrive(&self, failed: Vec<reqwest::RequestBuilder>) {
        for request in failed {
            let _ = self.pipeline.flusher.flush(Some(request)).await;
        }
    }

    /// Drains the aggregator and returns how many batches were still waiting. This removes
    /// them, so only call it when the test no longer needs those logs.
    async fn drain_pending_batches(&self) -> usize {
        self.pipeline
            .aggregator_handle
            .get_batches()
            .await
            .expect("aggregator reachable")
            .len()
    }

    /// Expected JSON for one log, using this function's ARN, service and tags.
    fn log(&self, message: &str, request_id: Option<&str>, timestamp: i64, status: &str) -> Value {
        self.log_with(
            message,
            json!({"arn": ARN, "request_id": request_id}),
            timestamp,
            status,
            &self.tags,
        )
    }

    fn log_with(
        &self,
        message: &str,
        lambda: Value,
        timestamp: i64,
        status: &str,
        tags: &str,
    ) -> Value {
        json!({
            "message": {
                "message": message,
                "lambda": lambda,
                "timestamp": timestamp,
                "status": status,
            },
            "hostname": ARN,
            "service": SERVICE,
            "ddtags": tags,
            "ddsource": "lambda",
        })
    }

    /// The two logs every `send_invocation_start()` produces, in shipping order. INIT_START
    /// arrives before any request_id exists, so it is held as an orphan and shipped right
    /// after START with START's request_id.
    fn invocation_start_logs(&self) -> Vec<Value> {
        vec![
            self.log(START_LINE, Some(REQUEST_ID), TS_START, "info"),
            self.log(INIT_START_LINE, Some(REQUEST_ID), TS_INIT_START, "info"),
        ]
    }
}

/// Counts `warn!`/`error!` events from the logs pipeline (bottlecap today, the shared crate
/// after the migration). Tests must run on the current-thread runtime so tasks spawned by
/// the pipeline run on the test thread and are counted.
#[derive(Clone, Default)]
struct LevelCounts(Arc<Mutex<HashMap<Level, usize>>>);

const PIPELINE_TARGETS: [&str; 2] = ["bottlecap", "datadog_logs_agent"];

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for LevelCounts {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let meta = event.metadata();
        if PIPELINE_TARGETS
            .iter()
            .any(|t| meta.target().starts_with(t))
        {
            *self
                .0
                .lock()
                .expect("level counts lock")
                .entry(*meta.level())
                .or_default() += 1;
        }
    }
}

impl LevelCounts {
    fn install() -> (Self, tracing::subscriber::DefaultGuard) {
        let counts = Self::default();
        let guard =
            tracing::subscriber::set_default(tracing_subscriber::registry().with(counts.clone()));
        (counts, guard)
    }

    fn get(&self, level: Level) -> usize {
        self.0
            .lock()
            .expect("level counts lock")
            .get(&level)
            .copied()
            .unwrap_or(0)
    }

    #[track_caller]
    fn assert(&self, warns: usize, errors: usize) {
        assert_eq!(
            (self.get(Level::WARN), self.get(Level::ERROR)),
            (warns, errors),
            "(warn!, error!) lines emitted by the logs pipeline"
        );
    }
}

// ---------------------------------------------------------------------------------------
// Log content
// ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn on_demand_invocation_lines() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send(&[
        INIT_START,
        START,
        &function_log("hello"),
        RUNTIME_DONE,
        REPORT,
    ])
    .await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.extend([
        h.log("hello", Some(REQUEST_ID), TS_FUNCTION, "info"),
        h.log(
            &format!("END RequestId: {REQUEST_ID}"),
            Some(REQUEST_ID),
            TS_RUNTIME_DONE,
            "info",
        ),
        h.log(
            &format!(
                "REPORT RequestId: {REQUEST_ID} Duration: 2599.40 ms Runtime Duration: 2599.00 ms \
                 Post Runtime Duration: 0.40 ms Billed Duration: 2600 ms Memory Size: 128 MB \
                 Max Memory Used: 94 MB Init Duration: 549.04 ms"
            ),
            Some(REQUEST_ID),
            TS_REPORT,
            "info",
        ),
    ]);
    assert_eq!(single_request_logs(&intake), Value::Array(expected));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn json_function_log_uses_level_and_extracts_ddtags() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    // ddtags is last so removing it cannot reorder the remaining keys.
    h.send(&[&function_log(json!({
        "level": "WARN",
        "message": "disk almost full",
        "ddtags": "team:logs"
    }))])
    .await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.push(h.log_with(
        r#"{"level":"WARN","message":"disk almost full"}"#,
        json!({"arn": ARN, "request_id": REQUEST_ID}),
        TS_FUNCTION,
        "warn",
        &format!("{},team:logs", h.tags),
    ));
    assert_eq!(single_request_logs(&intake), Value::Array(expected));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn non_object_json_and_extension_logs_are_shipped_as_text() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    h.send(&[
        &function_log("[1,2,3]"),
        &extension_log("extension says hi"),
    ])
    .await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.extend([
        h.log("[1,2,3]", Some(REQUEST_ID), TS_FUNCTION, "info"),
        h.log("extension says hi", Some(REQUEST_ID), TS_FUNCTION, "info"),
    ]);
    assert_eq!(single_request_logs(&intake), Value::Array(expected));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn function_log_before_start_is_attached_to_the_next_request() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send(&[INIT_START, &function_log("before start"), START])
        .await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.push(h.log("before start", Some(REQUEST_ID), TS_FUNCTION, "info"));
    assert_eq!(single_request_logs(&intake), Value::Array(expected));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn managed_instance_ships_logs_without_request_id_immediately() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::managed_instance(base_config(&intake));

    h.send(&[
        INIT_START,
        &function_log("sandbox log"),
        MANAGED_INSTANCE_REPORT,
    ])
    .await;
    assert!(h.flush().await.is_empty());

    let expected = json!([
        h.log(INIT_START_LINE, None, TS_INIT_START, "info"),
        h.log("sandbox log", None, TS_FUNCTION, "info"),
        h.log(
            &format!("REPORT RequestId: {REQUEST_ID} Runtime Duration: 1.15 ms"),
            Some(REQUEST_ID),
            TS_REPORT,
            "info"
        ),
    ]);
    assert_eq!(single_request_logs(&intake), expected);
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn processing_rules_exclude_and_mask() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(Config {
        logs_config_processing_rules: Some(vec![
            ProcessingRule {
                kind: Kind::ExcludeAtMatch,
                name: "drop-healthchecks".to_string(),
                pattern: "healthcheck".to_string(),
                replace_placeholder: None,
            },
            ProcessingRule {
                kind: Kind::MaskSequences,
                name: "mask-cards".to_string(),
                pattern: r"\d{4}-\d{4}".to_string(),
                replace_placeholder: Some("[masked]".to_string()),
            },
        ]),
        ..base_config(&intake)
    });

    h.send_invocation_start().await;
    h.send(&[
        &function_log("GET /healthcheck"),
        &function_log("card 1234-5678 used"),
    ])
    .await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.push(h.log("card [masked] used", Some(REQUEST_ID), TS_FUNCTION, "info"));
    assert_eq!(single_request_logs(&intake), Value::Array(expected));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn oom_line_is_shipped_like_any_other_log() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    h.send(&[&function_log("FATAL ERROR: JavaScript heap out of memory")])
        .await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.push(h.log(
        "FATAL ERROR: JavaScript heap out of memory",
        Some(REQUEST_ID),
        TS_FUNCTION,
        "info",
    ));
    assert_eq!(single_request_logs(&intake), Value::Array(expected));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn durable_sdk_log_carries_execution_context() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    h.send(&[&function_log(json!({
        "executionArn": "arn:aws:lambda:us-east-1:123456789012:function:f/durable-execution/my-exec/abc-123",
        "message": "step done"
    }))])
    .await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.push(h.log_with(
        r#"{"executionArn":"arn:aws:lambda:us-east-1:123456789012:function:f/durable-execution/my-exec/abc-123","message":"step done"}"#,
        json!({
            "arn": ARN,
            "request_id": REQUEST_ID,
            "durable_function.execution_id": "abc-123",
            "durable_function.execution_name": "my-exec"
        }),
        TS_FUNCTION,
        "info",
        &h.tags,
    ));
    assert_eq!(single_request_logs(&intake), Value::Array(expected));
    counts.assert(0, 0);
}

// ---------------------------------------------------------------------------------------
// Endpoints and request shape
// ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn datadog_intake_request_shape_uncompressed() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    let requests = intake.requests();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/api/v2/logs");
    assert_eq!(
        header_names(r),
        BTreeSet::from([
            "accept".to_string(),
            "content-type".to_string(),
            "dd-api-key".to_string(),
            "dd-protocol".to_string()
        ])
    );
    assert_eq!(r.header("DD-API-KEY"), Some(API_KEY));
    assert_eq!(r.header("DD-PROTOCOL"), Some("agent-json"));
    assert_eq!(r.header("Content-Type"), Some("application/json"));
    assert_eq!(r.header("Content-Encoding"), None);
    assert_eq!(r.json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn datadog_intake_request_shape_compressed() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(Config {
        logs_config_use_compression: true,
        ..base_config(&intake)
    });

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    let requests = intake.requests();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/api/v2/logs");
    assert_eq!(
        header_names(r),
        BTreeSet::from([
            "accept".to_string(),
            "content-encoding".to_string(),
            "content-type".to_string(),
            "dd-api-key".to_string(),
            "dd-protocol".to_string()
        ])
    );
    assert_eq!(r.header("DD-API-KEY"), Some(API_KEY));
    assert_eq!(r.header("DD-PROTOCOL"), Some("agent-json"));
    assert_eq!(r.header("Content-Type"), Some("application/json"));
    assert_eq!(r.header("Content-Encoding"), Some("zstd"));
    assert_eq!(r.json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn observability_pipelines_worker_request_shape() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(Config {
        observability_pipelines_worker_logs_enabled: true,
        observability_pipelines_worker_logs_url: format!("{}/opw/path", intake.base_url()),
        logs_config_use_compression: true, // OPW must still be uncompressed
        ..base_config(&intake)
    });

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    let requests = intake.requests();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/opw/path");
    assert_eq!(
        header_names(r),
        BTreeSet::from([
            "accept".to_string(),
            "content-type".to_string(),
            "dd-api-key".to_string()
        ])
    );
    assert_eq!(r.header("DD-API-KEY"), Some(API_KEY));
    assert_eq!(r.header("DD-PROTOCOL"), None);
    assert_eq!(r.header("Content-Type"), Some("application/json"));
    assert_eq!(r.header("Content-Encoding"), None);
    assert_eq!(r.json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn additional_endpoints_receive_the_same_logs() {
    let (counts, _guard) = LevelCounts::install();
    let primary = FakeLogsIntake::start_http().await;
    let extra_1 = FakeLogsIntake::start_https().await;
    let extra_2 = FakeLogsIntake::start_https().await;
    let mut h = Harness::new(Config {
        logs_config_use_compression: true,
        logs_config_additional_endpoints: vec![
            additional_endpoint(&extra_1, "extra-key-1"),
            additional_endpoint(&extra_2, "extra-key-2"),
        ],
        ..base_config(&primary)
    });

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    let expected = Value::Array(h.invocation_start_logs());
    assert_eq!(single_request_logs(&primary), expected);
    for (extra, key) in [(&extra_1, "extra-key-1"), (&extra_2, "extra-key-2")] {
        let requests = extra.requests();
        assert_eq!(requests.len(), 1);
        let r = &requests[0];
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/api/v2/logs");
        assert_eq!(
            header_names(r),
            BTreeSet::from([
                "accept".to_string(),
                "content-encoding".to_string(),
                "content-type".to_string(),
                "dd-api-key".to_string(),
                "dd-protocol".to_string()
            ])
        );
        assert_eq!(r.header("DD-API-KEY"), Some(key));
        assert_eq!(r.header("DD-PROTOCOL"), Some("agent-json"));
        assert_eq!(r.header("Content-Type"), Some("application/json"));
        assert_eq!(r.header("Content-Encoding"), Some("zstd"));
        assert_eq!(r.json(), expected);
    }
    counts.assert(0, 0);
}

/// With OPW enabled, only the primary goes to the worker. Additional endpoints are Datadog
/// intakes: `/api/v2/logs`, DD-PROTOCOL, and compression per `logs_config_use_compression`.
#[tokio::test(flavor = "current_thread")]
async fn observability_pipelines_worker_with_additional_endpoint() {
    let (counts, _guard) = LevelCounts::install();
    let primary = FakeLogsIntake::start_http().await;
    let extra = FakeLogsIntake::start_https().await;
    let mut h = Harness::new(Config {
        observability_pipelines_worker_logs_enabled: true,
        observability_pipelines_worker_logs_url: format!("{}/opw", primary.base_url()),
        logs_config_use_compression: true,
        logs_config_additional_endpoints: vec![additional_endpoint(&extra, "extra-key-1")],
        ..base_config(&primary)
    });

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    let primary_requests = primary.requests();
    assert_eq!(primary_requests.len(), 1);
    let p = &primary_requests[0];
    assert_eq!(p.path, "/opw");
    assert_eq!(p.header("DD-PROTOCOL"), None);
    assert_eq!(p.header("Content-Encoding"), None);
    assert_eq!(p.json(), Value::Array(h.invocation_start_logs()));
    let requests = extra.requests();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/api/v2/logs");
    assert_eq!(
        header_names(r),
        BTreeSet::from([
            "accept".to_string(),
            "content-encoding".to_string(),
            "content-type".to_string(),
            "dd-api-key".to_string(),
            "dd-protocol".to_string()
        ])
    );
    assert_eq!(r.header("DD-API-KEY"), Some("extra-key-1"));
    assert_eq!(r.header("DD-PROTOCOL"), Some("agent-json"));
    assert_eq!(r.header("Content-Type"), Some("application/json"));
    assert_eq!(r.header("Content-Encoding"), Some("zstd"));
    assert_eq!(r.json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 0);
}

/// Same as above with compression off: the additional endpoint still gets the Datadog
/// intake path and DD-PROTOCOL, uncompressed.
#[tokio::test(flavor = "current_thread")]
async fn observability_pipelines_worker_with_uncompressed_additional_endpoint() {
    let (counts, _guard) = LevelCounts::install();
    let primary = FakeLogsIntake::start_http().await;
    let extra = FakeLogsIntake::start_https().await;
    let mut h = Harness::new(Config {
        observability_pipelines_worker_logs_enabled: true,
        observability_pipelines_worker_logs_url: format!("{}/opw", primary.base_url()),
        logs_config_use_compression: false,
        logs_config_additional_endpoints: vec![additional_endpoint(&extra, "extra-key-1")],
        ..base_config(&primary)
    });

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    assert_eq!(primary.requests().len(), 1);
    let requests = extra.requests();
    assert_eq!(requests.len(), 1);
    let r = &requests[0];
    assert_eq!(r.path, "/api/v2/logs");
    assert_eq!(
        header_names(r),
        BTreeSet::from([
            "accept".to_string(),
            "content-type".to_string(),
            "dd-api-key".to_string(),
            "dd-protocol".to_string()
        ])
    );
    assert_eq!(r.header("DD-PROTOCOL"), Some("agent-json"));
    assert_eq!(r.header("Content-Encoding"), None);
    assert_eq!(r.json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 0);
}

/// The configured logs URL keeps its path; `/api/v2/logs` is appended to it.
#[tokio::test(flavor = "current_thread")]
async fn logs_dd_url_with_a_path_prefix_keeps_it() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(Config {
        logs_config_logs_dd_url: format!("{}/prefix", intake.base_url()),
        ..base_config(&intake)
    });

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    let requests = intake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/prefix/api/v2/logs");
    assert_eq!(requests[0].json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 0);
}

// ---------------------------------------------------------------------------------------
// Retries, redrive and drops
// ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn status_403_is_dropped_without_retry() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    intake.script_statuses(&[403]);
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    let failed = h.flush().await;

    assert!(failed.is_empty(), "403 is never redriven");
    assert_eq!(intake.requests().len(), 1);
    counts.assert(0, 1);
}

async fn assert_retried_three_times_then_redriven_once(status: u16) {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    intake.script_statuses(&[status; 6]);
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    let failed = h.flush().await;
    assert_eq!(failed.len(), 1, "one request handed back for redrive");
    assert_eq!(
        intake.requests().len(),
        3,
        "three attempts in the first flush"
    );

    h.redrive(failed).await;
    assert_eq!(
        intake.requests().len(),
        6,
        "three more attempts in the single redrive"
    );

    assert!(h.flush().await.is_empty());
    assert_eq!(
        intake.requests().len(),
        6,
        "nothing left to send after the redrive"
    );
    for r in intake.requests() {
        assert_eq!(r.json(), Value::Array(h.invocation_start_logs()));
    }
    counts.assert(0, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn status_500_is_retried_then_redriven_once() {
    assert_retried_three_times_then_redriven_once(500).await;
}

#[tokio::test(flavor = "current_thread")]
async fn status_400_is_retried_then_redriven_once() {
    assert_retried_three_times_then_redriven_once(400).await;
}

#[tokio::test(flavor = "current_thread")]
async fn status_401_is_retried_then_redriven_once() {
    assert_retried_three_times_then_redriven_once(401).await;
}

#[tokio::test(flavor = "current_thread")]
async fn status_413_is_retried_then_redriven_once() {
    assert_retried_three_times_then_redriven_once(413).await;
}

#[tokio::test(flavor = "current_thread")]
async fn status_429_is_retried_then_redriven_once() {
    assert_retried_three_times_then_redriven_once(429).await;
}

/// A redrive resends only the failed request. Logs that arrive in between wait for the
/// next regular flush, so a failure there still gets its own redrive.
#[tokio::test(flavor = "current_thread")]
async fn redrive_does_not_send_new_logs() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    intake.script_statuses(&[500; 3]);
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    let failed = h.flush().await;
    assert_eq!(failed.len(), 1);
    h.send(&[&function_log("after failure")]).await;

    h.redrive(failed).await;
    let requests = intake.requests();
    assert_eq!(requests.len(), 4, "only the failed request is re-sent");
    assert_eq!(requests[3].json(), Value::Array(h.invocation_start_logs()));

    assert!(h.flush().await.is_empty());
    let requests = intake.requests();
    assert_eq!(
        requests.len(),
        5,
        "the new log ships on the next regular flush"
    );
    assert_eq!(
        requests[4].json(),
        json!([h.log("after failure", Some(REQUEST_ID), TS_FUNCTION, "info")])
    );
    counts.assert(0, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn connection_error_is_handed_back() {
    let (counts, _guard) = LevelCounts::install();
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    }; // listener dropped: connections are refused
    let intake = FakeLogsIntake::start_http().await; // only for base_config; never called
    let mut h = Harness::new(Config {
        logs_config_logs_dd_url: format!("http://127.0.0.1:{closed_port}"),
        ..base_config(&intake)
    });

    h.send_invocation_start().await;
    let failed = h.flush().await;

    assert_eq!(failed.len(), 1);
    assert!(intake.requests().is_empty());
    counts.assert(0, 1);
}

/// A request that gets no answer within `flush_timeout` is retried, then handed back.
#[tokio::test(flavor = "current_thread")]
async fn request_timeout_is_retried_then_handed_back() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    intake.script_hangs(3);
    let mut h = Harness::new(Config {
        flush_timeout: 1,
        ..base_config(&intake)
    });

    h.send_invocation_start().await;
    let failed = tokio::time::timeout(Duration::from_secs(30), h.flush())
        .await
        .expect("flush must give up on its own within flush_timeout");

    assert_eq!(failed.len(), 1);
    assert_eq!(intake.requests().len(), 3, "three attempts, each timed out");

    h.redrive(failed).await;
    let requests = intake.requests();
    assert_eq!(requests.len(), 4, "the redrive is answered with 202");
    assert_eq!(requests[3].json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn additional_endpoint_failure_is_handed_back_for_redrive() {
    let (counts, _guard) = LevelCounts::install();
    let primary = FakeLogsIntake::start_http().await;
    let extra = FakeLogsIntake::start_https().await;
    extra.script_statuses(&[500, 500, 500]);
    let mut h = Harness::new(Config {
        logs_config_additional_endpoints: vec![additional_endpoint(&extra, "extra-key-1")],
        ..base_config(&primary)
    });

    h.send_invocation_start().await;
    let failed = h.flush().await;

    assert_eq!(
        failed.len(),
        1,
        "the additional endpoint's request is handed back"
    );
    assert_eq!(primary.requests().len(), 1);
    assert_eq!(extra.requests().len(), 3);

    h.redrive(failed).await;
    assert_eq!(primary.requests().len(), 1, "primary is not re-sent");
    assert_eq!(
        extra.requests().len(),
        4,
        "redrive goes to the additional endpoint"
    );
    assert_eq!(
        extra.requests()[3].header("DD-API-KEY"),
        Some("extra-key-1")
    );
    counts.assert(0, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn api_key_resolution_failure_drops_the_batch() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::with_unresolvable_api_key(base_config(&intake));

    h.send_invocation_start().await;
    let failed = h.flush().await;

    assert!(failed.is_empty());
    assert!(intake.requests().is_empty());
    assert_eq!(
        h.drain_pending_batches().await,
        0,
        "drained logs are dropped, not kept for later"
    );
    counts.assert(0, 1);

    // The error is logged on every flush, even with nothing to send.
    assert!(h.flush().await.is_empty());
    counts.assert(0, 2);
}

/// When the primary's API key cannot be resolved, only the primary is skipped: additional
/// endpoints still receive the logs with their own keys.
#[tokio::test(flavor = "current_thread")]
async fn api_key_resolution_failure_still_sends_to_additional_endpoints() {
    let (counts, _guard) = LevelCounts::install();
    let primary = FakeLogsIntake::start_http().await;
    let extra = FakeLogsIntake::start_https().await;
    let mut h = Harness::with_unresolvable_api_key(Config {
        logs_config_additional_endpoints: vec![additional_endpoint(&extra, "extra-key-1")],
        ..base_config(&primary)
    });

    h.send_invocation_start().await;
    assert!(h.flush().await.is_empty());

    assert!(primary.requests().is_empty());
    let requests = extra.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("DD-API-KEY"), Some("extra-key-1"));
    assert_eq!(requests[0].json(), Value::Array(h.invocation_start_logs()));
    counts.assert(0, 1);
}

// ---------------------------------------------------------------------------------------
// Batching
// ---------------------------------------------------------------------------------------

const MIB: usize = 1024 * 1024;

/// Split points are not pinned: only that every log arrives once, within the per-request
/// entry limit, and in order within each request.
#[tokio::test(flavor = "current_thread")]
async fn many_logs_are_split_into_batches_and_all_delivered() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));
    let messages: Vec<String> = (0..2_500).map(|i| format!("log {i:04}")).collect();

    h.send_invocation_start().await;
    for message in &messages {
        h.send(&[&function_log(message.as_str())]).await;
    }
    assert!(h.flush().await.is_empty());

    let mut delivered = Vec::new();
    for (i, request) in intake.requests().iter().enumerate() {
        let Value::Array(entries) = request.json() else {
            panic!("request {i} is not a JSON array");
        };
        assert!(
            entries.len() <= 1000,
            "request {i} has {} entries, more than 1000",
            entries.len()
        );
        let numbers: Vec<u32> = entries
            .iter()
            .filter_map(|e| e["message"]["message"].as_str()?.strip_prefix("log "))
            .map(|n| n.parse().expect("numbered log"))
            .collect();
        assert!(
            numbers.is_sorted_by(|a, b| a < b),
            "request {i}: numbered logs out of order"
        );
        delivered.extend(entries);
    }

    let mut expected = h.invocation_start_logs();
    expected.extend(
        messages
            .iter()
            .map(|m| h.log(m, Some(REQUEST_ID), TS_FUNCTION, "info")),
    );
    assert_each_log_delivered_once(delivered, expected);
    counts.assert(0, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn logs_over_5_mib_in_total_are_split_across_requests() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));
    let bigs: Vec<String> = ["a", "b", "c"].iter().map(|c| c.repeat(2 * MIB)).collect();

    h.send_invocation_start().await;
    for big in &bigs {
        h.send(&[&function_log(big.as_str())]).await;
    }
    assert!(h.flush().await.is_empty());

    let mut delivered = Vec::new();
    for (i, request) in intake.requests().iter().enumerate() {
        assert!(
            request.body.len() <= 5 * MIB,
            "request {i} body is {} bytes, more than 5 MiB",
            request.body.len()
        );
        let Value::Array(entries) = request.json() else {
            panic!("request {i} is not a JSON array");
        };
        delivered.extend(entries);
    }

    let mut expected = h.invocation_start_logs();
    expected.extend(
        bigs.iter()
            .map(|big| h.log(big, Some(REQUEST_ID), TS_FUNCTION, "info")),
    );
    assert_each_log_delivered_once(delivered, expected);
    counts.assert(3, 0);
}

// ---------------------------------------------------------------------------------------
// Oversized logs
// ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn log_between_1_and_5_mib_is_sent_with_one_warning() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));
    let big = "a".repeat(2 * MIB);

    h.send_invocation_start().await;
    h.send(&[&function_log(big.as_str())]).await;
    assert!(h.flush().await.is_empty());

    let mut expected = h.invocation_start_logs();
    expected.push(h.log(&big, Some(REQUEST_ID), TS_FUNCTION, "info"));
    let actual = single_request_logs(&intake);
    let expected = Value::Array(expected);
    assert!(
        actual == expected,
        "logs differ\n--- actual ---\n{}\n--- expected ---\n{}",
        summarize(&actual),
        summarize(&expected)
    );
    counts.assert(1, 0);
}

/// Pins a known bug on purpose (a later change fixes it): a log too large for any payload
/// stays at the head of the queue forever, so every log behind it is never sent.
#[tokio::test(flavor = "current_thread")]
async fn log_over_5_mib_blocks_later_logs() {
    let (counts, _guard) = LevelCounts::install();
    let intake = FakeLogsIntake::start_http().await;
    let mut h = Harness::new(base_config(&intake));

    h.send_invocation_start().await;
    h.send(&[&function_log("a".repeat(6 * MIB)), &function_log("after")])
        .await;
    assert!(h.flush().await.is_empty());
    assert!(h.flush().await.is_empty());

    let actual = single_request_logs(&intake);
    let expected = Value::Array(h.invocation_start_logs());
    assert!(
        actual == expected,
        "only the logs queued before the huge one are sent: logs differ\n--- actual ---\n{}\n--- expected ---\n{}",
        summarize(&actual),
        summarize(&expected)
    );
    counts.assert(0, 0);
}
