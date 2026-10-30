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

//! T6. Another extension or the runtime holds one of the fixed ports the
//! extension binds. The contract: no panic, the extension still registers and
//! answers `/next`, and it exits on SHUTDOWN. Only the listener on that port
//! is lost.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use harness::prelude::*;

const INVOKE_DEADLINE: Duration = secs(5);

enum Proto {
    Tcp,
}

/// Run one invocation and a shutdown with `port` held. Telemetry is delivered
/// only when the extension can receive it: when the held port is the
/// Telemetry API listener's own and the extension still subscribed with it,
/// no listener is up, so the scenario ends with a SHUTDOWN on the first
/// `/next` instead. `conflict_marker` is the log line the extension writes
/// when it handles the conflict without a panic.
async fn run_with_port_held(
    proto: Proto,
    port: u16,
    extra_env: &[(&str, &str)],
    conflict_marker: Option<&str>,
) -> (Scenario, BottlecapProcess) {
    // The scenario checks the fixed ports are free, so hold the port after it.
    let s = Scenario::start(FaultMode::Ok).await;
    let guard = match proto {
        Proto::Tcp => PortGuard::tcp_wait(port, secs(5)).await,
    }
    .unwrap_or_else(|| panic!("could not hold port {port}"));
    let mut bc = s.bottlecap().envs(extra_env.iter().copied()).spawn();

    let reached_next = s.wait_next_or_exit(&mut bc, 1, STARTUP_BUDGET).await;
    let reached_at = bc.uptime();
    let telemetry_possible = port != TELEMETRY_PORT || s.api.telemetry_port() != TELEMETRY_PORT;
    let mut flushed = false;
    if reached_next && telemetry_possible {
        let invoked = Instant::now();
        s.api.push_invoke("req-1", INVOKE_DEADLINE);
        if deliver_runtime_done(&s.api, "req-1", true).await.is_ok() {
            flushed = s
                .api
                .wait_for_next_calls(2, INVOKE_DEADLINE.saturating_sub(invoked.elapsed()))
                .await;
        }
    }
    let exit = s.shutdown(&mut bc).await;
    // Release the port only after the process is gone, so the conflict holds
    // for the process's whole life.
    drop(guard);
    eprintln!(
        "port {port} held: /next at {reached_at:?}, telemetry possible={telemetry_possible}, invocation flushed={flushed}, exit {exit:?} at {:?}, intake requests={}",
        bc.uptime(),
        s.intake.requests().len()
    );

    bc.assert_no_panic();
    assert!(
        reached_next,
        "with port {port} held, the extension never reached /next. API: {:?}\n{}",
        s.api.request_lines(),
        bc.log_tail(40)
    );
    assert!(
        flushed || !telemetry_possible,
        "with port {port} held, the invocation did not end within its {INVOKE_DEADLINE:?} deadline. API: {:?}\n{}",
        s.api.request_lines(),
        bc.log_tail(40)
    );
    assert!(
        exit.is_some_and(|e| e.success()),
        "with port {port} held, exit was {exit:?}. API: {:?}\nIntake:\n{}\n{}",
        s.api.request_lines(),
        s.intake.summary(),
        bc.log_tail(40)
    );
    if let Some(marker) = conflict_marker {
        assert!(
            bc.log_contains(marker),
            "port {port} was held but the extension did not report the conflict ({marker:?}); the guard did not conflict. {}",
            bc.log_tail(40)
        );
    }
    (s, bc)
}

/// 8124, the lifecycle listener: the bind error is returned and logged today.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn lifecycle_listener_port_8124_held() {
    run_with_port_held(
        Proto::Tcp,
        LIFECYCLE_PORT,
        &[],
        Some("Error starting lifecycle listener"),
    )
    .await;
}

/// 8126, the trace agent: the bind error is returned and logged today.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn trace_agent_port_8126_held() {
    run_with_port_held(
        Proto::Tcp,
        TRACE_AGENT_PORT,
        &[],
        Some("Error starting trace agent"),
    )
    .await;
}
