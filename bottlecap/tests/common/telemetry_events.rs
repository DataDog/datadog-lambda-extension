// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! Telemetry API records as `serde_json::Value`, for `FakeExtensionsApi::send_telemetry`.
//!
//! Thin wrappers over the shared corpus in `tests/fixtures/telemetry.rs`, the
//! single source for record shapes. Each test crate declares `mod fixtures;`
//! at its root so these wrappers can reach it.

use serde_json::Value;

use crate::fixtures::telemetry as corpus;

fn value(event: String) -> Value {
    serde_json::from_str(&event).expect("fixture event is valid JSON")
}

/// `platform.initStart` for an on-demand runtime.
pub fn platform_init_start() -> Value {
    value(corpus::init_start())
}

/// `platform.start` for `request_id`.
pub fn platform_start(request_id: &str) -> Value {
    value(corpus::platform_start(request_id))
}

/// `platform.runtimeDone` for `request_id`. Lambda omits `metrics` on some
/// failed invocations (a timeout), so `with_metrics = false` reproduces that.
pub fn platform_runtime_done(request_id: &str, status: &str, with_metrics: bool) -> Value {
    value(corpus::runtime_done_with(request_id, status, with_metrics))
}

/// `platform.report` for `request_id`.
pub fn platform_report(request_id: &str) -> Value {
    value(corpus::report(request_id))
}

/// A plain-text `function` log line.
pub fn function_log(message: &str) -> Value {
    value(corpus::event(
        60,
        "function",
        &Value::String(message.to_string()).to_string(),
    ))
}

/// A `platform.logsDropped` record, as Lambda sends when its buffer overflows.
pub fn platform_logs_dropped(dropped_records: u64) -> Value {
    value(corpus::logs_dropped(dropped_records))
}

/// An event of a type this extension does not know. Must never break a batch.
pub fn unknown_event() -> Value {
    value(corpus::unknown_event())
}
