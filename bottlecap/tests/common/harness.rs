// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! Fault-injection harness for process-level contract tests.
//!
//! Include it from a test binary as
//! `mod fixtures; #[path = "common/harness.rs"] mod harness; use harness::prelude::*;`.
//! The module must be named `harness`: the submodules refer to each other
//! through `crate::harness::...`, and `telemetry_events` reads the shared
//! corpus through `crate::fixtures`. See `tests/HARNESS.md`.

#[path = "bottlecap_process.rs"]
pub mod bottlecap_process;
#[path = "fake_extensions_api.rs"]
pub mod fake_extensions_api;
#[path = "fault_intake.rs"]
pub mod fault_intake;
#[path = "port_guard.rs"]
pub mod port_guard;
#[path = "telemetry_events.rs"]
pub mod telemetry_events;

#[allow(unused_imports)]
pub mod prelude {
    pub use super::bottlecap_process::{
        BottlecapBuilder, BottlecapProcess, EXIT_BUDGET, STARTUP_BUDGET, TEST_API_KEY,
    };
    pub use super::fake_extensions_api::{DEFAULT_TELEMETRY_PORT, FakeExtensionsApi};
    pub use super::fault_intake::{FaultIntake, FaultMode, IntakeRequest, SynBlackhole};
    pub use super::port_guard::{
        DOGSTATSD_UDP_PORT, LIFECYCLE_PORT, PROXY_PORT, PortGuard, TELEMETRY_PORT, TRACE_AGENT_PORT,
    };
    pub use super::telemetry_events::*;
    pub use super::{
        Scenario, deliver_runtime_done, incompressible_text, ms, msgpack_trace, post_trace, secs,
    };
    pub use std::time::{Duration, Instant};
}

use std::process::ExitStatus;
use std::time::Duration;

use bottlecap_process::BottlecapProcess;
use fake_extensions_api::FakeExtensionsApi;
use fault_intake::{FaultIntake, FaultMode};

pub const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

pub const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The deadline of the SHUTDOWN event, Lambda's default.
const SHUTDOWN_DEADLINE: Duration = secs(2);

/// A fake Extensions API and a fault intake, started together.
///
/// ```ignore
/// let s = Scenario::start(FaultMode::Ok).await;
/// let mut bc = s.bottlecap().spawn();
/// assert!(s.invoke("req-1", secs(10), 1).await);
/// ```
pub struct Scenario {
    pub api: FakeExtensionsApi,
    pub intake: FaultIntake,
}

impl Scenario {
    pub async fn start(mode: FaultMode) -> Self {
        port_guard::require_fixed_ports_free().await;
        let api = FakeExtensionsApi::start().await;
        let intake = FaultIntake::start(mode).await;
        Self { api, intake }
    }

    /// A process builder pointed at this scenario's fakes.
    pub fn bottlecap(&self) -> bottlecap_process::BottlecapBuilder {
        BottlecapProcess::builder(&self.api).intake(&self.intake.url())
    }

    /// Wait for `/next` call number `poll` to be open, then answer it with an
    /// INVOKE, as Lambda does. Returns `false` when the poll never opened.
    pub async fn invoke(&self, request_id: &str, deadline_in: Duration, poll: usize) -> bool {
        if !self
            .api
            .wait_for_next_calls(poll, bottlecap_process::STARTUP_BUDGET)
            .await
        {
            return false;
        }
        self.api.push_invoke(request_id, deadline_in);
        true
    }

    /// Wait for `/next` call number `n`, giving up early when the process
    /// exits (a crash at INIT should not cost the whole budget).
    pub async fn wait_next_or_exit(
        &self,
        bc: &mut BottlecapProcess,
        n: usize,
        budget: Duration,
    ) -> bool {
        let start = std::time::Instant::now();
        while self.api.next_calls() < n {
            if start.elapsed() > budget || !bc.is_running() {
                return self.api.next_calls() >= n;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    /// Answer the open `/next` with a SHUTDOWN ([`SHUTDOWN_DEADLINE`]) and
    /// wait for the process to exit. Returns the exit status, `None` when it
    /// had to be killed.
    pub async fn shutdown(&self, bc: &mut BottlecapProcess) -> Option<ExitStatus> {
        self.api.push_shutdown("spindown", SHUTDOWN_DEADLINE);
        bc.wait_for_exit(bottlecap_process::EXIT_BUDGET).await
    }

    /// [`shutdown`](Self::shutdown), then assert no panic and a clean exit
    /// within the SHUTDOWN deadline.
    pub async fn shutdown_cleanly(&self, bc: &mut BottlecapProcess) {
        let start = std::time::Instant::now();
        let exit = self.shutdown(bc).await;
        let took = start.elapsed();
        bc.assert_no_panic();
        assert!(
            exit.is_some_and(|e| e.success()),
            "exit {exit:?} after SHUTDOWN. API: {:?}\n{}",
            self.api.request_lines(),
            bc.log_tail(40)
        );
        assert!(
            took <= SHUTDOWN_DEADLINE,
            "exit took {took:?}, past the {SHUTDOWN_DEADLINE:?} SHUTDOWN deadline. {}",
            bc.log_tail(40)
        );
    }
}

/// `platform.start` then `platform.runtimeDone` for `request_id`, delivered
/// to the extension's Telemetry API listener.
pub async fn deliver_runtime_done(
    api: &FakeExtensionsApi,
    request_id: &str,
    with_metrics: bool,
) -> Result<(), String> {
    api.send_telemetry(
        &[
            telemetry_events::platform_start(request_id),
            telemetry_events::platform_runtime_done(request_id, "success", with_metrics),
        ],
        bottlecap_process::STARTUP_BUDGET,
    )
    .await
}

/// `n` bytes of alphanumerics from a fixed-seed LCG. Compresses to roughly
/// three quarters of its size, so a padded payload stays large on the wire.
pub fn incompressible_text(n: usize, seed: u64) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ALPHABET[((state >> 33) % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

/// A one-span trace in the v0.4 msgpack form tracers post to the trace agent.
/// `meta_bytes` pads a tag with incompressible text so a test can make the
/// payload large on the wire.
pub fn msgpack_trace(trace_id: u64, meta_bytes: usize) -> Vec<u8> {
    let now_ns = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    )
    .unwrap_or(i64::MAX);
    let padding = incompressible_text(meta_bytes, trace_id);
    let span = serde_json::json!({
        "trace_id": trace_id,
        "span_id": trace_id,
        "parent_id": 0u64,
        "name": "aws.lambda",
        "resource": "test-handler",
        "service": "test-service",
        "type": "serverless",
        "start": now_ns,
        "duration": 1_000_000i64,
        "error": 0,
        "meta": {"_dd.origin": "lambda", "harness.padding": padding},
        "metrics": {"_sampling_priority_v1": 1.0}
    });
    rmp_serde::to_vec_named(&vec![vec![span]]).expect("msgpack trace")
}

/// Post a v0.4 trace payload to the trace agent on 8126, retrying until it
/// is up or `budget` passes. Returns the HTTP status.
pub async fn post_trace(body: Vec<u8>, budget: Duration) -> Result<u16, String> {
    // Test-only client towards the local trace agent; the FIPS adapter is for production.
    #[allow(clippy::disallowed_methods)]
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    let start = std::time::Instant::now();
    let mut last: String;
    loop {
        let result = client
            .post(format!(
                "http://127.0.0.1:{}/v0.4/traces",
                port_guard::TRACE_AGENT_PORT
            ))
            .header("content-type", "application/msgpack")
            .header("x-datadog-trace-count", "1")
            .header("datadog-meta-lang", "harness")
            .header("datadog-meta-tracer-version", "0.0.0")
            .body(body.clone())
            .send()
            .await;
        match result {
            Ok(r) => return Ok(r.status().as_u16()),
            Err(e) => last = e.to_string(),
        }
        if start.elapsed() > budget {
            return Err(format!(
                "trace agent not reachable within {budget:?}: {last}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
