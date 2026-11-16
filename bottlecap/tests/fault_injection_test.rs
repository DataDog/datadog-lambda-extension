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

//! Fault-injection contract tests: the real `bottlecap` binary against a fake
//! Extensions API and an intake that misbehaves. Each test asserts what Lambda
//! or the customer would observe: the Invoke phase ends before the deadline,
//! the faulted request reaches the intake, and nothing panics.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use harness::prelude::*;
use libdd_trace_protobuf::pb;
use prost::Message as _;

const TRACES_PATH: &str = "/api/v0.2/traces";
const LOGS_PATH: &str = "/api/v2/logs";

/// The root span with `trace_id` from an accepted trace request.
fn decoded_root_span(request: &IntakeRequest, trace_id: u64) -> pb::Span {
    let body = request.decoded_body().expect("decode intake body");
    let payload = pb::AgentPayload::decode(body.as_slice()).expect("decode AgentPayload");
    payload
        .tracer_payloads
        .into_iter()
        .flat_map(|payload| payload.chunks)
        .flat_map(|chunk| chunk.spans)
        .find(|span| span.trace_id == trace_id)
        .expect("the trace must reach the intake")
}

/// The function log [`end_invocation_and_wait`] sends for `request_id`.
fn log_text(request_id: &str) -> String {
    format!("log for {request_id}")
}

/// Deliver `platform.start`, a log and `runtimeDone` for `request_id`, then
/// return whether `/next` call number `next_call` arrived by `deadline`, the
/// INVOKE's.
async fn end_invocation_and_wait(
    s: &Scenario,
    request_id: &str,
    cold_start: bool,
    next_call: usize,
    deadline: Instant,
) -> bool {
    deliver_start(&s.api, request_id, cold_start).await.unwrap();
    s.api
        .send_telemetry(&[function_log(&log_text(request_id))], STARTUP_BUDGET)
        .await
        .unwrap();
    deliver_runtime_done(&s.api, request_id, true)
        .await
        .unwrap();
    s.api.wait_for_next_calls(next_call, deadline).await
}

/// T2 (no hang). A 503 storm must not hold the Invoke phase: retries are
/// bounded and `/next` is called before the deadline.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn status_503_storm_does_not_hold_next() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().spawn();
    let deadline = s
        .invoke("req-1", secs(10), 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));

    s.intake.set_mode(FaultMode::Status(503));
    let released = end_invocation_and_wait(&s, "req-1", true, 2, deadline).await;
    // Read before the fault clears: the end-of-invocation flush sends the log.
    let logs_faulted = s.intake.requests_to(LOGS_PATH).iter().any(|r| {
        r.status == 503
            && r.api_key() == Some(TEST_API_KEY)
            && r.body_text().unwrap().contains(&log_text("req-1"))
    });

    s.intake.set_mode(FaultMode::Ok);
    let exit = s.shutdown(&mut bc).await;
    bc.assert_no_panic();
    assert!(
        released,
        "a 503 storm held /next past the invoke deadline. {}",
        bc.log_tail(40)
    );
    assert!(
        logs_faulted,
        "the storm never hit the logs intake with the function log and the API key. Intake:\n{}",
        s.intake.summary()
    );
    assert!(exit.is_some_and(|e| e.success()), "exit {exit:?}");
}

/// T7 (trace). A trace payload the intake rejects with 413 must be dropped,
/// not redriven forever; the next normal trace still flows and nothing
/// panics. Passes today: the current code drops every non-`Err` result.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn oversized_trace_is_dropped_and_later_traces_flow() {
    let s = Scenario::start(FaultMode::Ok).await;
    s.intake.set_reject_over(Some(1_000_000));
    let mut bc = s.bottlecap().spawn();
    let deadline = s
        .invoke("req-1", secs(10), 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));

    let status = post_trace(msgpack_trace(1, 4_000_000), STARTUP_BUDGET)
        .await
        .unwrap();
    eprintln!("trace agent answered {status} to a 4 MB span");
    let released = end_invocation_and_wait(&s, "req-1", true, 2, deadline).await;
    assert!(
        released,
        "the oversized trace held /next. {}",
        bc.log_tail(40)
    );
    let rejected = s
        .intake
        .wait_for(
            1,
            |r| {
                r.path.contains(TRACES_PATH)
                    && r.status == 413
                    && r.api_key() == Some(TEST_API_KEY)
                    && r.trace_ids().unwrap().contains(&1)
            },
            deadline,
        )
        .await;

    s.intake.set_reject_over(None);
    let deadline = s
        .invoke("req-2", secs(10), 2, deadline)
        .await
        .expect("no second /next poll");
    post_trace(msgpack_trace(2, 0), STARTUP_BUDGET)
        .await
        .unwrap();
    let released = end_invocation_and_wait(&s, "req-2", false, 3, deadline).await;
    assert!(released, "no /next after the second invocation");
    let later_trace = s
        .intake
        .wait_for(
            1,
            |r| {
                r.path.contains(TRACES_PATH)
                    && r.status == 202
                    && r.api_key() == Some(TEST_API_KEY)
                    && r.trace_ids().unwrap().contains(&2)
            },
            deadline,
        )
        .await;

    s.shutdown_cleanly(&mut bc).await;
    assert!(
        rejected,
        "the 4 MB trace (ID 1) never reached the intake with the API key by its invoke deadline (accepted by the agent: {status}). Intake:\n{}",
        s.intake.summary()
    );
    // The flusher retries the 413 within its flush today; the contract is
    // that the 4 MB trace is never sent again once the rejection is lifted.
    // Matched by size on purpose, not decoded: a negative check must not
    // pass because a body failed to decode.
    let redriven = s
        .intake
        .requests()
        .iter()
        .any(|r| r.path.contains(TRACES_PATH) && r.body_len > 1_000_000 && r.status != 413);
    assert!(
        !redriven,
        "the rejected trace was redriven after the fault cleared. Intake:\n{}",
        s.intake.summary()
    );
    assert!(
        later_trace,
        "trace ID 2, after the rejected one, never arrived with the API key by its invoke deadline. Intake:\n{}\n{}",
        s.intake.summary(),
        bc.log_tail(40)
    );
}

/// `close_mid_body`: the intake sends a 2xx with headers and part of the body,
/// then closes. The request reached the intake, so this checks only that the
/// extension does not hold `/next` or crash. Passes today.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn intake_closing_mid_body_does_not_hold_next() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().spawn();
    let deadline = s
        .invoke("req-1", secs(10), 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));

    s.intake.set_mode(FaultMode::CloseMidBody);
    let released = end_invocation_and_wait(&s, "req-1", true, 2, deadline).await;
    // Read before the fault clears: the end-of-invocation flush sends the log.
    let logs_faulted = s.intake.requests_to(LOGS_PATH).iter().any(|r| {
        r.mode == "close_mid_body"
            && r.api_key() == Some(TEST_API_KEY)
            && r.body_text().unwrap().contains(&log_text("req-1"))
    });

    s.intake.set_mode(FaultMode::Ok);
    let exit = s.shutdown(&mut bc).await;
    bc.assert_no_panic();
    assert!(
        released,
        "a truncating intake held /next past the invoke deadline. {}",
        bc.log_tail(40)
    );
    assert!(
        logs_faulted,
        "the truncating mode never served the function log with the API key. Intake:\n{}",
        s.intake.summary()
    );
    assert!(exit.is_some_and(|e| e.success()), "exit {exit:?}");
}

/// An errored priority-0 trace is marked for backend retention even when the
/// backend owns stats, without changing its sampling priority.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn backend_owned_stats_rescues_errored_sampled_out_trace() {
    let s = Scenario::start(FaultMode::Ok).await;
    // Backend owns stats: the extension computes none and the tracer sends no
    // `Datadog-Client-Computed-Stats` header (`post_trace` sends none).
    let mut bc = s
        .bottlecap()
        .env("DD_LAMBDA_EXTENSION_COMPUTE_STATS", "false")
        .env("DD_APM_ERROR_TPS", "10")
        .spawn();
    let deadline = s
        .invoke("req-1", secs(10), 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));

    let mut traces = fixtures::traces::make_traces(1, 1, false);
    let root = &mut traces[0][0];
    root.error = 1;
    root.metrics
        .insert("_sampling_priority_v1".to_string(), 0.0);
    let trace_id = root.trace_id;
    let status = post_trace(fixtures::traces::v04_msgpack(&traces), STARTUP_BUDGET)
        .await
        .unwrap();
    eprintln!("trace agent answered {status} to the errored priority-0 trace");

    let released = end_invocation_and_wait(&s, "req-1", true, 2, deadline).await;
    let accepted = s
        .intake
        .wait_for(
            1,
            |r| {
                r.path.contains(TRACES_PATH)
                    && r.status == 202
                    && r.api_key() == Some(TEST_API_KEY)
                    && r.trace_ids().unwrap().contains(&trace_id)
            },
            deadline,
        )
        .await;

    s.shutdown_cleanly(&mut bc).await;

    assert!(
        released,
        "the errored sampled-out trace held /next. {}",
        bc.log_tail(40)
    );
    assert!(
        accepted,
        "the errored priority-0 trace never reached the intake with the API key (trace agent answered {status}). Intake:\n{}",
        s.intake.summary()
    );
    let request = s
        .intake
        .requests_to(TRACES_PATH)
        .into_iter()
        .find(|r| r.status == 202 && r.api_key() == Some(TEST_API_KEY))
        .expect("accepted trace request");
    let root = decoded_root_span(&request, trace_id);
    assert!(
        root.error != 0,
        "the decoded root span must still be errored. Intake:\n{}",
        s.intake.summary()
    );
    let errors_sr = root.metrics.get("_dd.errors_sr").copied();
    assert!(
        errors_sr.is_some_and(|rate| rate > 0.0),
        "the backend-owned errored trace must be rescued with _dd.errors_sr > 0, got {errors_sr:?}. Intake:\n{}\n{}",
        s.intake.summary(),
        bc.log_tail(40)
    );
    assert_eq!(
        root.metrics.get("_sampling_priority_v1").copied(),
        Some(0.0),
        "the rescue must not change the sampling priority. Intake:\n{}",
        s.intake.summary()
    );
}
