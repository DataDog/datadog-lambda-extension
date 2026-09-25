// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Standalone mock Datadog APM intake for local debugging.
//!
//! Runs the shared `datadog-mock-intake` crate with request summaries enabled
//! and accepts the same APM endpoints the Lambda extension flushes to. Point a
//! tracer or the `bottlecap-test-mode` trace processor at it and inspect
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
        "mock-intake: DSM     endpoint POST {}/api/v0.1/pipeline_stats",
        intake.base_url()
    );
    println!("mock-intake: waiting for SIGINT or SIGTERM to shut down");

    wait_for_shutdown()
        .await
        .context("mock-intake: failed to wait for shutdown signal")?;
    drop(intake);
    println!("mock-intake: shut down");
    Ok(())
}

fn parse_options() -> anyhow::Result<MockIntakeOptions> {
    let port = match std::env::var("MOCK_INTAKE_PORT") {
        Ok(raw) => raw
            .parse::<u16>()
            .with_context(|| format!("mock-intake: invalid MOCK_INTAKE_PORT value '{raw}', expected a port number between 0 and 65535"))?,
        Err(std::env::VarError::NotPresent) => 8127,
        Err(std::env::VarError::NotUnicode(raw)) => {
            bail!("mock-intake: MOCK_INTAKE_PORT is not valid Unicode: {}", raw.display())
        }
    };

    let fail_stats_first_n = match std::env::var("MOCK_INTAKE_FAIL_STATS_FIRST_N") {
        Ok(raw) => raw
            .parse::<usize>()
            .with_context(|| format!("mock-intake: invalid MOCK_INTAKE_FAIL_STATS_FIRST_N value '{raw}', expected a non-negative integer"))?,
        Err(std::env::VarError::NotPresent) => 0,
        Err(std::env::VarError::NotUnicode(raw)) => {
            bail!("mock-intake: MOCK_INTAKE_FAIL_STATS_FIRST_N is not valid Unicode: {}", raw.display())
        }
    };

    let dump_dir = match std::env::var("MOCK_INTAKE_DUMP_DIR") {
        Ok(raw) => Some(PathBuf::from(raw)),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(raw)) => {
            bail!(
                "mock-intake: MOCK_INTAKE_DUMP_DIR is not valid Unicode: {}",
                raw.display()
            )
        }
    };

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
