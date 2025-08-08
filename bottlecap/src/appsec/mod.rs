use std::env;

use crate::config::Config;

pub mod processor;

/// Determines whether the Serverless App & API Protection features are enabled.
#[must_use]
pub const fn is_enabled(cfg: &Config) -> bool {
    cfg.serverless_appsec_enabled
}

/// Determines whether APM is only used as a transport for App & API Protection,
/// instead of being used for tracing as well.
#[must_use]
pub fn is_standalone() -> bool {
    env::var("DD_APM_TRACING_ENABLED").is_ok_and(|s| s.to_lowercase() == "true")
}
