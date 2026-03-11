use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::Config;
use bottlecap::event_bus::EventBus;
use bottlecap::extension::telemetry::events::TelemetryEvent;
use bottlecap::logs::agent::LogsAgent;
use bottlecap::tags::provider::Provider;
use datadog_log_agent::{AggregatorService, FlusherMode, LogFlusher, LogFlusherConfig};
use httpmock::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

mod common;

#[tokio::test]
async fn test_logs() {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        a => a,
    };
    let dd_api_key = "my_test_key";

    // New flat LogEntry format: top-level message, hostname, lambda attrs flattened
    let regexp_message =
        r#""message":"START RequestId: 459921b5-681c-4a96-beb0-81e0aa586026 Version: $LATEST""#;
    let regexp_request_id = r#""request_id":"459921b5-681c-4a96-beb0-81e0aa586026""#;
    let regexp_hostname = r#""hostname":"test-arn""#;
    let regexp_compute_state = r#"_dd.compute_stats:1"#;
    let regexp_arch = format!(r#"architecture:{}"#, arch);
    let regexp_function_arn = r#"function_arn:test-arn"#;
    let regexp_extension_version = r#"dd_extension_version"#;

    let server = MockServer::start();
    let hello_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/logs")
            .header("DD-API-KEY", dd_api_key)
            .header("Content-Type", "application/json")
            .body_contains(regexp_message)
            .body_contains(regexp_request_id)
            .body_contains(regexp_hostname)
            .body_contains(regexp_compute_state)
            .body_contains(regexp_arch)
            .body_contains(regexp_function_arn)
            .body_contains(regexp_extension_version);

        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });

    let arc_conf = Arc::new(Config {
        http_protocol: Some("http1".to_string()),
        logs_config_use_compression: false,
        logs_config_logs_dd_url: server.url(""),
        ..Config::default()
    });
    let tags_provider = Arc::new(Provider::new(
        Arc::clone(&arc_conf),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
    ));

    let (_, bus_tx) = EventBus::run();

    let (logs_aggr_service, logs_aggr_handle) = AggregatorService::new();
    tokio::spawn(async move {
        logs_aggr_service.run().await;
    });

    let (mut logs_agent, logs_agent_tx) = LogsAgent::new(
        tags_provider,
        Arc::clone(&arc_conf),
        bus_tx.clone(),
        logs_aggr_handle.clone(),
        false,
    );

    let client = bottlecap::http::get_client(&Arc::clone(&arc_conf));
    let flusher_config = LogFlusherConfig {
        api_key: dd_api_key.to_string(),
        site: "datadoghq.com".to_string(),
        mode: FlusherMode::ObservabilityPipelinesWorker {
            url: format!("{}/api/v2/logs", server.url("")),
        },
        additional_endpoints: Vec::new(),
        use_compression: false,
        compression_level: 3,
        flush_timeout: Duration::from_secs(5),
    };
    let logs_flusher = LogFlusher::new(flusher_config, client, logs_aggr_handle);

    let telemetry_events: Vec<TelemetryEvent> = serde_json::from_str(
        r#"[{"time":"2022-10-21T14:05:03.165Z","type":"platform.start","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","version":"$LATEST","tracing":{"spanId":"24cd7d670fa455f0","type":"X-Amzn-Trace-Id","value":"Root=1-6352a70e-1e2c502e358361800241fd45;Parent=35465b3a9e2f7c6a;Sampled=1"}}}]"#)
        .map_err(|e| e.to_string()).expect("Failed parsing telemetry events");

    let sender = logs_agent_tx.clone();

    for an_event in telemetry_events {
        sender
            .send(an_event.clone())
            .await
            .expect("Failed sending telemetry events");
    }

    logs_agent.sync_consume().await;

    let _ = logs_flusher.flush(vec![]).await;

    hello_mock.assert();
}
