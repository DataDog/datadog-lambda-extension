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

pub mod config;
pub mod event_bus;
pub mod events;
pub mod fips;
pub mod http_client;
pub mod lifecycle;
pub mod logger;
pub mod logs;
pub mod lwa;
pub mod metrics;
pub mod otlp;
pub mod proc;
pub mod proxy;
pub mod secrets;
pub mod tags;
pub mod telemetry;
pub mod traces;

use std::{env, io};

pub const EXTENSION_HOST: &str = "0.0.0.0";
pub const EXTENSION_NAME: &str = "datadog-agent";
pub const EXTENSION_FEATURES: &str = "accountId";
pub const EXTENSION_NAME_HEADER: &str = "Lambda-Extension-Name";
pub const EXTENSION_ID_HEADER: &str = "Lambda-Extension-Identifier";
pub const EXTENSION_ACCEPT_FEATURE_HEADER: &str = "Lambda-Extension-Accept-Feature";
pub const EXTENSION_ROUTE: &str = "2020-01-01/extension";
pub const LAMBDA_RUNTIME_SLUG: &str = "lambda";

// todo: consider making this configurable
pub const FLUSH_RETRY_COUNT: usize = 3;

// todo: make sure we can override those with environment variables
pub const DOGSTATSD_PORT: u16 = 8125;

pub const TELEMETRY_SUBSCRIPTION_ROUTE: &str = "2022-07-01/telemetry";
// todo(astuyve) should be 8124 on /lambda/logs but
// telemetry is implemented on a raw socket now and
// does not multiplex routes on the same port.
pub const TELEMETRY_PORT: u16 = 8999;

/// Return the base URL for the lambda runtime API
///
/// # Errors
///
/// Function will error if the envar `AWS_LAMBDA_RUNTIME_API` is not set in the
/// environment.
pub fn base_url(route: &str) -> io::Result<String> {
    Ok(format!(
        "http://{}/{}",
        env::var("AWS_LAMBDA_RUNTIME_API")
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?,
        route
    ))
}
