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
