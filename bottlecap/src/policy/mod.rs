//! Policy evaluation module for filtering telemetry based on configured policies.
//!
//! This module integrates the `policy-rs` library to provide policy-based
//! filtering of logs and traces. Policies can be loaded from files or
//! HTTP endpoints and support keep, drop, sample, and rate-limit actions.

mod evaluator;
mod matchable;

pub use evaluator::PolicyEvaluator;
pub use matchable::SpanWrapper;
