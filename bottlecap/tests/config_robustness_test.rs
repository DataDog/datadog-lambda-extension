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

//! T5. Configuration fields get a value of the wrong type. The expected
//! contract, one bad value costs only that field and `DD_API_KEY` survives,
//! is a design decision for the config owner (today the whole environment
//! source is dropped). The INIT-panic cases are not a policy question.
//!

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use harness::prelude::*;

const INVOKE_DEADLINE: Duration = secs(10);
const SKETCHES_PATH: &str = "/api/beta/sketches";

/// Run one invocation with `extra` in the env and report whether its
/// invocation metric reached the intake with `DD_API_KEY` by the invoke
/// deadline, then shut down cleanly.
async fn runs_and_flushes_with_api_key(extra: &[(&str, &str)]) -> bool {
    let s = Scenario::start(FaultMode::Ok).await;
    let mut bc = s.bottlecap().envs(extra.iter().copied()).spawn();
    let deadline = s
        .invoke("req-1", INVOKE_DEADLINE, 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));
    // Lambda sends `platform.initStart` on a cold start. Until it arrives the
    // extension holds the invocation metric, as it cannot yet tell a durable
    // function.
    s.api
        .send_telemetry(&[platform_init_start()], STARTUP_BUDGET)
        .await
        .unwrap();
    deliver_runtime_done(&s.api, "req-1", true).await.unwrap();
    assert!(
        s.api.wait_for_next_calls(2, deadline).await,
        "no second /next within the {INVOKE_DEADLINE:?} invoke deadline. {}",
        bc.log_tail(40)
    );
    let with_key = s
        .intake
        .wait_for(
            1,
            |r| {
                r.path.contains(SKETCHES_PATH)
                    && r.api_key() == Some(TEST_API_KEY)
                    && r.metric_names()
                        .unwrap()
                        .iter()
                        .any(|n| n == INVOCATIONS_METRIC)
            },
            deadline,
        )
        .await;
    s.shutdown_cleanly(&mut bc).await;
    if !with_key {
        eprintln!(
            "no {INVOCATIONS_METRIC} reached the intake with DD_API_KEY. Intake:\n{}\n{}",
            s.intake.summary(),
            bc.log_tail(40)
        );
    }
    with_key
}

/// One wrong field at a time, for the fields the review names. Each must
/// cost only itself. One test per case so none runs into the slow-test
/// timeout. Fields with a lenient deserializer, and every extension field,
/// already cost only themselves: those tests run in CI and keep it so.
macro_rules! one_wrong_field_costs_only_itself {
    ($name:ident, $key:literal, $value:literal) => {
        #[tokio::test(flavor = "multi_thread")]
        #[serial_test::serial(bottlecap_ports)]
        async fn $name() {
            assert!(
                runs_and_flushes_with_api_key(&[($key, $value)]).await,
                "{}={} cost more than itself",
                $key,
                $value
            );
        }
    };
}

one_wrong_field_costs_only_itself!(
    wrong_type_flush_strategy,
    "DD_SERVERLESS_FLUSH_STRATEGY",
    "1000"
);
one_wrong_field_costs_only_itself!(wrong_type_flush_timeout, "DD_FLUSH_TIMEOUT", "soon");
one_wrong_field_costs_only_itself!(
    wrong_type_processing_rules,
    "DD_LOGS_CONFIG_PROCESSING_RULES",
    "1"
);
one_wrong_field_costs_only_itself!(wrong_type_log_level, "DD_LOG_LEVEL", "[a]");
