// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// Integration tests are exempt from the production lint policy in Cargo.toml.
#![allow(
    unreachable_pub,
    clippy::pedantic,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used
)]

//! Lifecycle contract tests: the real `bottlecap` binary against a fake
//! Extensions API. Each test asserts what Lambda would observe (calls to
//! `/next`, process exit, time to exit), never internal state.
//!
//! Every test here spawns the binary, which binds fixed ports, so each one
//! carries `#[serial_test::serial(bottlecap_ports)]` (for `cargo test`) and
//! the binary is in the `fixed-ports` nextest group.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use harness::prelude::*;

const INVOKE_DEADLINE: Duration = secs(10);

/// Happy path, also a self-test of the harness: an invocation ends, the
/// extension flushes to the intake and calls `/next` again, then exits on
/// SHUTDOWN with no panic.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn invoke_then_shutdown_exits_cleanly() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().spawn();

    let deadline = s
        .invoke("req-1", INVOKE_DEADLINE, 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));
    deliver_start(&s.api, "req-1", true).await.unwrap();
    s.api
        .send_telemetry(&[function_log("hello from the function")], STARTUP_BUDGET)
        .await
        .unwrap();
    deliver_runtime_done(&s.api, "req-1", true).await.unwrap();
    assert!(
        s.api.wait_for_next_calls(2, deadline).await,
        "no second /next within the {INVOKE_DEADLINE:?} invoke deadline. {}",
        bc.log_tail(40)
    );

    // Read the intake before SHUTDOWN: its final flush would hide a missing
    // end-of-invocation flush.
    // Enhanced metrics are distributions, so they land on the sketches path.
    let metrics = [
        s.intake.requests_to("/api/beta/sketches"),
        s.intake.requests_to("/api/v2/series"),
    ]
    .concat();
    let metrics_keyed = metrics.iter().all(|r| r.api_key() == Some(TEST_API_KEY));
    let invocations_flushed = metrics.iter().any(|r| {
        r.metric_names()
            .unwrap()
            .iter()
            .any(|n| n == INVOCATIONS_METRIC)
    });
    let log_flushed = s.intake.requests_to("/api/v2/logs").iter().any(|r| {
        r.api_key() == Some(TEST_API_KEY)
            && r.body_text().unwrap().contains("hello from the function")
    });
    s.shutdown_cleanly(&mut bc).await;

    // The end-of-invocation flush reached the intake, each payload with the
    // configured key.
    assert!(
        invocations_flushed,
        "no {INVOCATIONS_METRIC} flushed before SHUTDOWN. Intake:\n{}",
        s.intake.summary()
    );
    assert!(
        metrics_keyed,
        "a metrics payload without the API key. Intake:\n{}",
        s.intake.summary()
    );
    assert!(
        log_flushed,
        "the function log was not flushed with the API key before SHUTDOWN. Intake:\n{}",
        s.intake.summary()
    );
}

/// Lambda starts the runtime, and the tracer's first `/info` call, once every
/// extension has registered, not at the extension's first `/next`. The fake
/// probes the trace agent port while it handles register, so the trace agent
/// must already be listening by then.
async fn assert_trace_agent_listens_before_register(managed_instance: bool) {
    let s = Scenario::start(FaultMode::Ok).await;
    let builder = s.bottlecap();
    let mut bc = if managed_instance {
        builder.managed_instance().spawn()
    } else {
        builder.spawn()
    };

    assert!(
        s.api.wait_for_next_calls(1, bc.startup_deadline()).await,
        "no /next poll. {}",
        bc.log_tail(40)
    );
    assert_eq!(
        s.api.trace_agent_listening_at_register(),
        Some(true),
        "the trace agent was not listening on 8126 when the extension registered. {}",
        bc.log_tail(40)
    );
    // Only the startup order matters here. The process is killed rather than
    // shut down because the On-Demand loop does not yet act on a SHUTDOWN that
    // answers the first `/next`.
    bc.assert_no_panic();
    bc.kill();
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn trace_agent_listens_before_register() {
    assert_trace_agent_listens_before_register(false).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn trace_agent_listens_before_register_managed_instance() {
    assert_trace_agent_listens_before_register(true).await;
}

/// The .NET tracer consumes the trace headers returned by start-invocation.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn express_event_without_redrive_count_returns_trace_context() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().spawn();
    let deadline = s
        .invoke("req-express", INVOKE_DEADLINE, 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));
    deliver_start(&s.api, "req-express", true).await.unwrap();

    let context: serde_json::Value =
        serde_json::from_str(&fixtures::events::payload("step_function_express_event")).unwrap();
    // Test-only client towards the local lifecycle listener.
    #[allow(clippy::disallowed_methods)]
    let client = reqwest::Client::builder().build().unwrap();
    let response = tokio::time::timeout_at(
        deadline.into(),
        client
            .post(format!(
                "http://127.0.0.1:{LIFECYCLE_PORT}/lambda/start-invocation"
            ))
            .header("lambda-runtime-aws-request-id", "req-express")
            .header("x-datadog-tracing-enabled", "false")
            .json(&serde_json::json!({"_datadog": context}))
            .send(),
    )
    .await
    .expect("start-invocation exceeded the invocation deadline")
    .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.headers().get("x-datadog-trace-id"),
        Some(&reqwest::header::HeaderValue::from_static(
            "4284077050596024822"
        )),
        "Express trace context was not returned to the tracer. {}",
        bc.log_tail(40)
    );
    assert_eq!(
        response.headers().get("x-datadog-tags"),
        Some(&reqwest::header::HeaderValue::from_static(
            "_dd.p.tid=f1a18940a433d46"
        )),
        "Express trace ID high bits were not returned to the tracer. {}",
        bc.log_tail(40)
    );

    deliver_runtime_done(&s.api, "req-express", true)
        .await
        .unwrap();
    assert!(
        s.api.wait_for_next_calls(2, deadline).await,
        "no second /next within the invocation deadline. {}",
        bc.log_tail(40)
    );
    s.shutdown_cleanly(&mut bc).await;
}

/// Invocations before the RSS baseline. In Linux runs of this test, RSS levels off by invocation 500.
const MEMORY_WARMUP: usize = 500;
/// Invocations measured after the baseline.
const MEMORY_MEASURED: usize = 3000;
/// Allowed RSS growth over the measured invocations. A leak of 2 KiB per invocation grows
/// about 6 MiB in 3,000 invocations. Without a leak, RSS moves about 1 MiB.
const MEMORY_GROWTH_LIMIT_KB: u64 = 3 * 1024;

/// Runs warm invocations, calling `per_invocation` with each request ID and deadline between
/// `platform.start` and `platform.runtimeDone`. On Linux, asserts that RSS grows by at most
/// [`MEMORY_GROWTH_LIMIT_KB`] after the warm-up. Then shuts down cleanly.
async fn assert_memory_stays_bounded(
    s: &Scenario,
    mut bc: BottlecapProcess,
    per_invocation: impl AsyncFn(&str, Instant),
) {
    // Without a memory measurement, a few invocations still check the calls and the shutdown.
    let measured = if BottlecapProcess::can_measure_memory() {
        MEMORY_MEASURED
    } else {
        10
    };
    let mut poll_by = bc.startup_deadline();
    let mut rss_baseline = None;
    for n in 1..=MEMORY_WARMUP + measured {
        let request_id = format!("req-{n}");
        let deadline = s
            .invoke(&request_id, INVOKE_DEADLINE, n, poll_by)
            .await
            .unwrap_or_else(|| panic!("no /next poll {n}. {}", bc.log_tail(40)));
        deliver_start(&s.api, &request_id, n == 1).await.unwrap();
        per_invocation(&request_id, deadline).await;
        deliver_runtime_done(&s.api, &request_id, true)
            .await
            .unwrap();
        poll_by = deadline;
        if n == MEMORY_WARMUP {
            rss_baseline = bc.rss_kb();
        }
    }
    assert!(
        s.api
            .wait_for_next_calls(MEMORY_WARMUP + measured + 1, poll_by)
            .await,
        "no /next after the last invocation. {}",
        bc.log_tail(40)
    );

    if BottlecapProcess::can_measure_memory() {
        let baseline = rss_baseline.expect("no RSS after the warm-up");
        let end = bc.rss_kb().expect("no RSS after the last invocation");
        let growth = end.saturating_sub(baseline);
        assert!(
            growth <= MEMORY_GROWTH_LIMIT_KB,
            "RSS grew {growth} KiB over {measured} invocations ({baseline} -> {end} KiB). {}",
            bc.log_tail(40)
        );
    }
    s.shutdown_cleanly(&mut bc).await;
}

/// With universal instrumentation, the tracer calls start-invocation and end-invocation on
/// every invocation. Over many warm invocations, the extension's memory must stay bounded.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn universal_instrumentation_memory_stays_bounded() {
    let s = Scenario::start(FaultMode::Ok).await;
    let bc = s.bottlecap().spawn();
    // Test-only client towards the local lifecycle listener.
    #[allow(clippy::disallowed_methods)]
    let client = reqwest::Client::builder().build().unwrap();
    let event: serde_json::Value =
        serde_json::from_str(&fixtures::events::payload("api_gateway_http_event")).unwrap();

    assert_memory_stays_bounded(&s, bc, async |request_id, deadline| {
        for (path, body) in [
            ("start-invocation", event.clone()),
            ("end-invocation", serde_json::json!({"statusCode": 200})),
        ] {
            let response = tokio::time::timeout_at(
                deadline.into(),
                client
                    .post(format!("http://127.0.0.1:{LIFECYCLE_PORT}/lambda/{path}"))
                    .header("lambda-runtime-aws-request-id", request_id)
                    .json(&body)
                    .send(),
            )
            .await
            .unwrap_or_else(|_| panic!("{path} for {request_id} exceeded the invocation deadline"))
            .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
        }
    })
    .await;
}
