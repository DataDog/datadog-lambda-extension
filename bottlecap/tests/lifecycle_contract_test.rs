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

/// Happy path, also a self-test of the harness: an invocation ends, the
/// extension flushes to the intake and calls `/next` again, then exits on
/// SHUTDOWN with no panic.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn invoke_then_shutdown_exits_cleanly() {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().spawn();

    assert!(
        s.invoke("req-1", secs(10), 1).await,
        "no /next poll. {}",
        bc.log_tail(40)
    );
    s.api
        .send_telemetry(&[function_log("hello from the function")], STARTUP_BUDGET)
        .await
        .unwrap();
    deliver_runtime_done(&s.api, "req-1", true).await.unwrap();
    assert!(
        s.api.wait_for_next_calls(2, secs(10)).await,
        "no second /next after runtimeDone. {}",
        bc.log_tail(40)
    );

    // Read the intake before SHUTDOWN: its final flush would hide a missing
    // end-of-invocation flush.
    let with_key = s
        .intake
        .requests()
        .iter()
        .filter(|r| r.api_key() == Some(TEST_API_KEY))
        .count();
    // Enhanced metrics are distributions, so they land on the sketches path.
    let metrics = s.intake.count("/api/beta/sketches") + s.intake.count("/api/v2/series");
    s.shutdown_cleanly(&mut bc).await;

    // The end-of-invocation flush reached the intake with the configured key.
    assert!(
        with_key > 0,
        "no intake request carried the API key before SHUTDOWN. Intake:\n{}",
        s.intake.summary()
    );
    assert!(
        metrics > 0,
        "no metrics flushed before SHUTDOWN. Intake:\n{}",
        s.intake.summary()
    );
}
