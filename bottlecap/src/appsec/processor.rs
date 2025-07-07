use std::sync::Arc;

use crate::appsec::{is_enabled, is_standalone};
use crate::config::Config;

use libddwaf::object::{WAFMap, WAFOwned};
use libddwaf::{Builder, Config as WAFConfig, Handle};
use tracing::{debug, info};

pub struct Processor {
    _config: Arc<Config>,
    _handle: Handle,
    diagnostics: WAFOwned<WAFMap>,
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

        let Some(mut builder) = Builder::new(&WAFConfig::default()) else {
            return Err("Failed to create WAF builder".into());
        };

        let rules = Self::get_rules(&config)?;
        let mut diagnostics = WAFOwned::<WAFMap>::default();
        if !builder.add_or_update_config("rules", &rules, Some(&mut diagnostics)) {
            return Err("Failed to add ruleset to the WAF builder".into());
        }

        let Some(handle) = builder.build() else {
            return Err("Failed to build WAF instance".into());
        };

        Ok(Self {
            _config: config.clone(),
            _handle: handle,
            diagnostics,
        })
    }

    fn get_rules(config: &Config) -> Result<WAFMap, Box<dyn std::error::Error>> {
        // Default on recommended rules
        match &config.appsec_rules {
            None => {
                let default_rules = include_bytes!("rules.json");
                Ok(serde_json::from_slice(default_rules)?)
            }
            Some(path) => {
                let rules = std::fs::File::open(path)?;
                Ok(serde_json::from_reader(rules)?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    use super::*;

    #[test]
    fn test_new_with_default_config() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let proc = Processor::new(Arc::new(config)).expect("Should not fail");
        match proc.diagnostics.get(b"ruleset_version") {
            None => panic!("Ruleset version should be present in diagnostics"),
            Some(version) => assert_ne!(version.to_str().expect("Should be a valid string"), ""),
        }
    }
}
