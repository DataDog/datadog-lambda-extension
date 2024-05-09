//! Crate for the `bottlecap` project
pub mod config;
pub mod event_bus;
pub mod events;
pub mod lifecycle;
pub mod logger;
pub mod logs;
pub mod metrics;
pub mod telemetry;

use std::{env, io};

pub const EXTENSION_HOST: &str = "0.0.0.0";
pub const EXTENSION_NAME: &str = "datadog-agent";
pub const EXTENSION_FEATURES: &str = "accountId";
pub const EXTENSION_NAME_HEADER: &str = "Lambda-Extension-Name";
pub const EXTENSION_ID_HEADER: &str = "Lambda-Extension-Identifier";
pub const EXTENSION_ACCEPT_FEATURE_HEADER: &str = "Lambda-Extension-Accept-Feature";
pub const EXTENSION_ROUTE: &str = "2020-01-01/extension";

// todo: make sure we can override those with environment variables
pub const DOGSTATSD_PORT: u16 = 8185;

pub const TELEMETRY_SUBSCRIPTION_ROUTE: &str = "2022-07-01/telemetry";
pub const TELEMETRY_PORT: u16 = 8124;

pub fn base_url(route: &str) -> io::Result<String> {
    Ok(format!(
        "http://{}/{}",
        env::var("AWS_LAMBDA_RUNTIME_API")
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?,
        route
    ))
}
