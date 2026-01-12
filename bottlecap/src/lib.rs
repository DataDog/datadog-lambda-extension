//! Crate for the `bottlecap` project
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(clippy::unwrap_used)]
#![deny(unused_extern_crates)]
#![deny(unused_allocation)]
#![deny(unused_assignments)]
#![deny(unused_comparisons)]
#![deny(unreachable_pub)]
#![deny(missing_copy_implementations)]
// #![deny(missing_debug_implementations)]

// TODO rmove these over time
#![allow(missing_docs)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::needless_pass_by_value)]
// Allow use of the `coverage_nightly` attribute
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod appsec;
pub mod config;
pub mod event_bus;
pub mod extension;
pub mod fips;
pub mod http;
pub mod lifecycle;
pub mod logger;
pub mod logs;
pub mod lwa;
pub mod metrics;
pub mod otlp;
pub mod policy;
pub mod proc;
pub mod proxy;
pub mod secrets;
pub mod tags;
pub mod traces;

pub const LAMBDA_RUNTIME_SLUG: &str = "lambda";

// todo: consider making this configurable
pub const FLUSH_RETRY_COUNT: usize = 3;

// todo: make sure we can override those with environment variables
pub const DOGSTATSD_PORT: u16 = 8125;
