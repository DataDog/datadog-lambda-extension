use std::sync::Arc;

use crate::appsec::{get_rules, is_enabled, is_standalone};
use crate::config::Config;

use libddwaf::WafInstance;
use tracing::{debug, info};

pub struct Processor {
    _config: Arc<Config>,
    _handle: WafInstance,
}

impl Processor {
    pub fn new(config: Arc<Config>) -> Result<Self, Box<dyn std::error::Error>> {
        if !is_enabled(&config) {
            return Err("AppSec is not enabled".into());
        }
        debug!("Starting ASM processor");

        if is_standalone() {
            info!(
                "Starting ASM in standalone mode. APM tracing will be disabled for this service."
            );
        }

        // Check if ASM can run properly
        // wafHealth

        let _rules = get_rules(&config).unwrap_or_default();

        Err("Not implemented".into())
    }
}
