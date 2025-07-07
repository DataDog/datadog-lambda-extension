use std::env;

use crate::config::Config;

pub mod processor;

#[must_use]
pub const fn is_enabled(config: &Config) -> bool {
    config.serverless_appsec_enabled
}

/// Reads the `DD_APM_TRACING_ENABLED` environment variable to determine whether ASM runs in
/// standalone mode.
///
/// This is a direct port of the Go logic and that is why we are not using the
/// `config` crate.
#[must_use]
pub fn is_standalone() -> bool {
    let apm_tracing_enabled = env::var("DD_APM_TRACING_ENABLED");
    let is_set = apm_tracing_enabled.is_ok();

    let enabled = apm_tracing_enabled
        .unwrap_or("false".to_string())
        .to_lowercase()
        == "true";

    is_set && enabled
}

pub fn get_rules(config: &Config) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    // Default on recommended rules
    match &config.appsec_rules {
        None => {
            let default_rules = include_bytes!("rules.json");
            Ok(default_rules.to_vec())
        }
        Some(path) => {
            let rules = std::fs::read(path)?;
            Ok(rules)
        }
    }
}
