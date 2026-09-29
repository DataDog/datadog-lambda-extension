// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Standalone mock Datadog APM intake for local debugging.
//!
//! Runs the shared `datadog-mock-intake` crate with request summaries enabled
//! and accepts the same APM endpoints the Lambda extension flushes to. Point a
//! tracer or the `bottlecap-test-mode` trace processor (added in
//! DataDog/datadog-lambda-extension#1216) at it and inspect
//! decoded payloads and JSON dumps locally. See `bottlecap/README.md` for usage.
//!
//! Environment variables:
//!
//! | Variable                        | Default | Purpose                                          |
//! |---------------------------------|---------|--------------------------------------------------|
//! | `MOCK_INTAKE_PORT`              | `8127`  | Loopback listener port (`0` = OS-assigned)       |
//! | `MOCK_INTAKE_FAIL_STATS_FIRST_N`| `0`     | Return HTTP 500 for the first N stats attempts   |
//! | `MOCK_INTAKE_DUMP_DIR`          | unset   | Write one JSON envelope per decoded request here |

#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(clippy::unwrap_used)]
#![deny(unused_extern_crates)]
#![deny(unused_allocation)]
#![deny(unused_assignments)]
#![deny(unused_comparisons)]
#![deny(unreachable_pub)]
#![deny(missing_copy_implementations)]
#![deny(missing_debug_implementations)]

use std::path::PathBuf;

use anyhow::{Context, bail};
use datadog_mock_intake::{MockIntake, MockIntakeOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let options = parse_options()?;
    let intake = MockIntake::start_with_options(options)
        .await
        .context("mock-intake: failed to start")?;

    println!("mock-intake: listening on {}", intake.base_url());
    println!("mock-intake: stats   endpoint POST {}", intake.stats_url());
    println!("mock-intake: traces  endpoint POST {}", intake.traces_url());
    println!(
        "mock-intake: DSM     endpoint POST {}",
        intake.pipeline_stats_url()
    );
    println!("mock-intake: waiting for SIGINT or SIGTERM to shut down");

    wait_for_shutdown()
        .await
        .context("mock-intake: failed to wait for shutdown signal")?;
    drop(intake);
    println!("mock-intake: shut down");
    Ok(())
}

/// Parse an environment variable via `FromStr`, falling back to `default` when
/// unset. `expected` describes the accepted format for the error message.
fn parse_env_or<T: std::str::FromStr>(name: &str, default: T, expected: &str) -> anyhow::Result<T> {
    let value = match std::env::var(name) {
        Ok(raw) => raw.parse::<T>().map_err(|_| {
            anyhow::anyhow!("mock-intake: invalid {name} value '{raw}', expected {expected}")
        })?,
        Err(std::env::VarError::NotPresent) => default,
        Err(std::env::VarError::NotUnicode(raw)) => {
            bail!(
                "mock-intake: {name} is not valid Unicode: {}",
                raw.display()
            )
        }
    };
    Ok(value)
}

fn parse_options() -> anyhow::Result<MockIntakeOptions> {
    let port = parse_env_or(
        "MOCK_INTAKE_PORT",
        8127u16,
        "a port number between 0 and 65535",
    )?;
    let fail_stats_first_n = parse_env_or(
        "MOCK_INTAKE_FAIL_STATS_FIRST_N",
        0usize,
        "a non-negative integer",
    )?;

    let dump_dir = std::env::var_os("MOCK_INTAKE_DUMP_DIR").map(PathBuf::from);

    Ok(MockIntakeOptions {
        port,
        request_summaries: true,
        fail_stats_first_n,
        dump_dir,
    })
}

async fn wait_for_shutdown() -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate())
        .context("mock-intake: failed to register SIGTERM handler")?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
    Ok(())
}
