use std::time::Duration;

use crate::appsec::{is_enabled, is_standalone, payload};
use crate::config::Config;

use bytes::Bytes;
use libddwaf::object::{WAFMap, WAFOwned};
use libddwaf::{Builder, Config as WAFConfig, Context, Handle};
use tracing::{debug, info, warn};
/// The App & API Protection processor.
///
/// It is used to try to identify invoke requests that are supported, extract the relevant data from
/// the request payload, and evaluate in-app WAF rules against that data.
pub struct Processor {
    handle: Handle,
    diagnostics: WAFOwned<WAFMap>,
    waf_timeout: Duration,
}

impl Processor {
    /// Creates a new [`Processor`] instance using the provided [`Config`].
    ///
    /// # Errors
    /// - If [`Config::serverless_appsec_enabled`] is `false`;
    /// - If the [`Config::appsec_rules`] points to a non-existent file;
    /// - If the [`Config::appsec_rules`] points to a file that is not a valid JSON-encoded ruleset;
    /// - If the in-app WAF fails to initialize, integrate the ruleset, or build the WAF instance.
    pub fn new(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        if !is_enabled(config) {
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

        let rules = Self::get_rules(config)?;
        let mut diagnostics = WAFOwned::<WAFMap>::default();
        if !builder.add_or_update_config("rules", &rules, Some(&mut diagnostics)) {
            return Err("Failed to add ruleset to the WAF builder".into());
        }

        let Some(handle) = builder.build() else {
            return Err("Failed to build WAF instance".into());
        };

        Ok(Self {
            handle,
            diagnostics,
            waf_timeout: config.appsec_waf_timeout,
        })
    }

    /// Process the `/runtime/invocation/next` payload, which is sent to Lambda to request an
    /// invocation event.
    #[must_use]
    pub fn process_invocation_next(&self, body: &Bytes) -> Option<ProcessorContext> {
        let address_data = payload::extract_request_address_data(body)?;

        let mut context = ProcessorContext {
            waf_context: self.handle.new_context(),
            waf_timeout: self.waf_timeout,
        };

        context.run(address_data);
        Some(context)
    }

    /// Process the `/runtime/invocation/<request_id>/response>` payload, which is sent to Lambda
    /// after the invocation has run to completion, to provide the result of the invocation.
    pub fn process_invocation_response(
        &self,
        context: Option<&mut ProcessorContext>,
        body: &Bytes,
    ) {
        let Some(context) = context else {
            return;
        };

        let Some(address_data) = payload::extract_response_address_data(body) else {
            return;
        };

        context.run(address_data);
    }

    /// Parses the App & API Protection ruleset from the provided [Config], falling back to the
    /// default built-in ruleset if the [Config] has [None].
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

/// The WAF context for a single invocation.
///
/// This is used to process both the request & response of a given invocation.
pub struct ProcessorContext {
    waf_context: Context,
    waf_timeout: Duration,
}
impl ProcessorContext {
    /// Evaluates the in-app WAF rules against the provided address data.
    fn run(&mut self, address_data: WAFMap) {
        let result = match self
            .waf_context
            .run(Some(address_data), None, self.waf_timeout)
        {
            Ok(result) => result,
            Err(e) => {
                warn!("Failed to evalute in-app WAF rules against request: {e}");
                return;
            }
        };

        todo!("Process {result:?}")
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
        let proc = Processor::new(&config).expect("Should not fail");
        match proc.diagnostics.get(b"ruleset_version") {
            None => panic!("Ruleset version should be present in diagnostics"),
            Some(version) => assert_ne!(version.to_str().expect("Should be a valid string"), ""),
        }
    }
}
