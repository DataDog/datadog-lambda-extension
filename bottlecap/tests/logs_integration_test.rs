use bottlecap::config::Config;
use bottlecap::event_bus::bus::EventBus;
use bottlecap::logs::{agent::LogsAgent, flusher::Flusher as LogsFlusher};
use bottlecap::tags::provider::Provider;
use bottlecap::telemetry::events::TelemetryEvent;
use bottlecap::telemetry::listener::TcpError;
use bottlecap::LAMBDA_RUNTIME_SLUG;
use httpmock::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

mod common;

#[tokio::test]
async fn test_logs() {
    let dd_api_key = "my_test_key";

    // protobuf is using hashmap, can't set a btreemap to have sorted keys. Using multiple regexp since
    // Can't do look around since -> error: look-around, including look-ahead and look-behind, is not supported
    let regexp_message = r#"[{"message":{"message":"START RequestId: 459921b5-681c-4a96-beb0-81e0aa586026 Version: $LATEST","lambda":{"arn":"test-arn","request_id":"459921b5-681c-4a96-beb0-81e0aa586026"},"timestamp":1666361103165,"status":"info"},"hostname":"test-arn","service":"","#;
    let regexp_compute_state = r#".*_dd.compute_stats:1.*"#;
    let regexp_arch = r#".*architecture:x86_64.*"#;
    let regexp_function_arn = r#".*function_arn:test-arn.*"#;

    let server = MockServer::start();
    let hello_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/logs")
            .header("DD-API-KEY", dd_api_key)
            .header("Content-Type", "application/json")
            .body_contains(regexp_message)
            .body_matches(Regex::new(regexp_compute_state).unwrap())
            .body_matches(Regex::new(regexp_arch).unwrap())
            .body_matches(Regex::new(regexp_function_arn).unwrap());

        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });

    let arc_conf = Arc::new(Config::default());
    let tags_provider = Arc::new(Provider::new(
        Arc::clone(&arc_conf),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
    ));

    let bus = EventBus::run();
    let mut logs_agent =
        LogsAgent::new(tags_provider, Arc::clone(&arc_conf), bus.get_sender_copy());
    let logs_flusher = LogsFlusher::new(
        dd_api_key.to_string(),
        Arc::clone(&logs_agent.aggregator),
        server.base_url(),
    );

    let telemetry_events: Vec<TelemetryEvent> = serde_json::from_str(
        r#"[{"time":"2022-10-21T14:05:03.165Z","type":"platform.start","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","version":"$LATEST","tracing":{"spanId":"24cd7d670fa455f0","type":"X-Amzn-Trace-Id","value":"Root=1-6352a70e-1e2c502e358361800241fd45;Parent=35465b3a9e2f7c6a;Sampled=1"}}}]"#)
        .map_err(|e| TcpError::Parse(e.to_string())).expect("Failed parsing telemetry events");

    logs_agent
        .get_sender_copy()
        .send(telemetry_events)
        .await
        .expect("Failed sending telemetry events");

    logs_agent.sync_consume().await;

    let res = logs_flusher.flush().await;

    assert!(res.contains_key("l0"));

    hello_mock.assert();
}
