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
//! Extensions API and an intake that misbehaves (T1 to T4, T7, T8 of the
//! review's test plan). Each test asserts what Lambda or the customer would
//! observe: the Invoke phase ends before the deadline, data is redriven after
//! the fault clears, nothing panics, memory stays bounded.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use harness::prelude::*;

const TRACES_PATH: &str = "/api/v0.2/traces";
/// Deliver a log and `runtimeDone` for `request_id`, then wait for `/next`
/// call number `next_call` until `budget` after `invoked`, when the INVOKE
/// was queued. Returns `(released, time since the INVOKE)`.
async fn end_invocation_and_wait(
    s: &Scenario,
    request_id: &str,
    next_call: usize,
    invoked: Instant,
    budget: Duration,
) -> (bool, Duration) {
    s.api
        .send_telemetry(
            &[function_log(&format!("log for {request_id}"))],
            STARTUP_BUDGET,
        )
        .await
        .unwrap();
    deliver_runtime_done(&s.api, request_id, true)
        .await
        .unwrap();
    let released = s
        .api
        .wait_for_next_calls(next_call, budget.saturating_sub(invoked.elapsed()))
        .await;
    (released, invoked.elapsed())
}

/// T2 (no hang). A 503 storm must not hold the Invoke phase: retries are
/// bounded and `/next` is called before the deadline.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn status_503_storm_does_not_hold_next() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().spawn();
    assert!(
        s.invoke("req-1", secs(10), 1).await,
        "no /next poll. {}",
        bc.log_tail(40)
    );
    let invoked = Instant::now();

    s.intake.set_mode(FaultMode::Status(503));
    let (released, took) = end_invocation_and_wait(&s, "req-1", 2, invoked, secs(10)).await;

    s.intake.set_mode(FaultMode::Ok);
    let exit = s.shutdown(&mut bc).await;
    bc.assert_no_panic();
    assert!(
        released,
        "a 503 storm held /next for {took:?}. {}",
        bc.log_tail(40)
    );
    assert!(
        s.intake.requests().iter().any(|r| r.status == 503),
        "the storm never hit the intake. Intake:\n{}",
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
    assert!(
        s.invoke("req-1", secs(10), 1).await,
        "no /next poll. {}",
        bc.log_tail(40)
    );
    let invoked = Instant::now();

    let status = post_trace(msgpack_trace(1, 4_000_000), STARTUP_BUDGET)
        .await
        .unwrap();
    eprintln!("trace agent answered {status} to a 4 MB span");
    let (released, _) = end_invocation_and_wait(&s, "req-1", 2, invoked, secs(10)).await;
    assert!(
        released,
        "the oversized trace held /next. {}",
        bc.log_tail(40)
    );
    let rejected = s
        .intake
        .wait_for(
            1,
            |r| r.path.contains(TRACES_PATH) && r.status == 413,
            secs(5),
        )
        .await;

    s.intake.set_reject_over(None);
    assert!(s.invoke("req-2", secs(10), 2).await, "no second /next poll");
    let invoked = Instant::now();
    post_trace(msgpack_trace(2, 0), STARTUP_BUDGET)
        .await
        .unwrap();
    let (released, _) = end_invocation_and_wait(&s, "req-2", 3, invoked, secs(10)).await;
    assert!(released, "no /next after the second invocation");
    let later_trace = s
        .intake
        .wait_for(
            1,
            |r| r.path.contains(TRACES_PATH) && r.status == 202 && r.body_len < 1_000_000,
            secs(10),
        )
        .await;

    s.shutdown_cleanly(&mut bc).await;
    assert!(
        rejected,
        "the 4 MB trace never reached the intake (accepted by the agent: {status}). Intake:\n{}",
        s.intake.summary()
    );
    // The flusher retries the 413 within its flush today; the contract is
    // that the 4 MB trace is never sent again once the rejection is lifted.
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
        "the trace after the rejected one never arrived. Intake:\n{}\n{}",
        s.intake.summary(),
        bc.log_tail(40)
    );
}

/// `close_mid_body`: the intake sends headers and part of the body, then
/// closes. The extension must treat it as a failed request and keep flowing.
/// Passes today.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn intake_closing_mid_body_does_not_hold_next() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().spawn();
    assert!(
        s.invoke("req-1", secs(10), 1).await,
        "no /next poll. {}",
        bc.log_tail(40)
    );
    let invoked = Instant::now();

    s.intake.set_mode(FaultMode::CloseMidBody);
    let (released, took) = end_invocation_and_wait(&s, "req-1", 2, invoked, secs(10)).await;

    s.intake.set_mode(FaultMode::Ok);
    let exit = s.shutdown(&mut bc).await;
    bc.assert_no_panic();
    assert!(
        released,
        "a truncating intake held /next for {took:?}. {}",
        bc.log_tail(40)
    );
    assert!(
        s.intake
            .requests()
            .iter()
            .any(|r| r.mode == "close_mid_body"),
        "the truncating mode never served a request. Intake:\n{}",
        s.intake.summary()
    );
    assert!(exit.is_some_and(|e| e.success()), "exit {exit:?}");
}
