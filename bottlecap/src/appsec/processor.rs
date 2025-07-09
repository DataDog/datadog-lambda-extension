use std::collections::HashMap;
use std::time::Duration;

use crate::appsec::{is_enabled, is_standalone, payload};
use crate::config::Config;

use bytes::Bytes;
use libddwaf::object::{WafMap, WafOwned};
use libddwaf::{Builder, Config as WAFConfig, Context, Handle, RunResult};
use tracing::{debug, info, warn};
/// The App & API Protection processor.
///
/// It is used to try to identify invoke requests that are supported, extract the relevant data from
/// the request payload, and evaluate in-app WAF rules against that data.
pub struct Processor {
    handle: Handle,
    _diagnostics: WafOwned<WafMap>,
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
        let mut diagnostics = WafOwned::<WafMap>::default();
        if !builder.add_or_update_config("rules", &rules, Some(&mut diagnostics)) {
            return Err("Failed to add ruleset to the WAF builder".into());
        }

        let Some(handle) = builder.build() else {
            return Err("Failed to build WAF instance".into());
        };

        Ok(Self {
            handle,
            _diagnostics: diagnostics,
            waf_timeout: config.appsec_waf_timeout,
        })
    }

    /// Process the `/runtime/invocation/next` payload, which is sent to Lambda to request an
    /// invocation event.
    #[must_use]
    pub async fn process_invocation_next(&self, body: &Bytes) -> Option<ProcessorContext> {
        let address_data = payload::extract_request_address_data(body).await?;

        let mut context = ProcessorContext {
            waf_context: self.handle.new_context(),
            waf_timeout: self.waf_timeout,
            duration: Duration::ZERO,
            timeouts: 0,
            keep: false,
            attributes: HashMap::new(),
            events: Vec::new(),
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
    fn get_rules(config: &Config) -> Result<WafMap, Box<dyn std::error::Error>> {
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
    duration: Duration,
    timeouts: u32,
    keep: bool,
    attributes: HashMap<String, String>,
    events: Vec<String>,
}
impl ProcessorContext {
    /// Evaluates the in-app WAF rules against the provided address data.
    fn run(&mut self, address_data: WafMap) {
        let timeout = self.waf_timeout.saturating_sub(self.duration);
        if timeout == Duration::ZERO {
            warn!(
                "appsec: WAF timeout already reached, not evaluating request with {address_data:?}"
            );
            return;
        }

        let result = match self.waf_context.run(Some(address_data), None, timeout) {
            Ok(RunResult::Match(result) | RunResult::NoMatch(result)) => result,
            Err(e) => {
                warn!("Failed to evalute in-app WAF rules against request: {e}");
                return;
            }
        };

        self.duration += result.duration();
        if result.timeout() {
            self.timeouts += 1;
        }
        if result.keep() {
            self.keep = true;
        }
        if let Some(attributes) = result.attributes() {
            self.attributes.reserve(attributes.len());
            for attr in attributes.iter() {
                let Ok(key) = attr.key_str() else { continue };
                let value = if let Some(value) = attr.to_str() {
                    value.to_string()
                } else if let Some(value) = attr.to_u64() {
                    value.to_string()
                } else if let Some(value) = attr.to_i64() {
                    value.to_string()
                } else if let Some(value) = attr.to_f64() {
                    value.to_string()
                } else if let Some(value) = attr.to_bool() {
                    value.to_string()
                } else {
                    debug!("appsec: unsupported attribute produced by the WAF: {attr:?}");
                    continue;
                };
                self.attributes.insert(key.to_string(), value);
            }
        }
        if let Some(events) = result.events() {
            self.events.reserve(events.len());
            for event in events.iter() {
                let enc = match serde_json::to_string(event) {
                    Ok(enc) => enc,
                    Err(e) => {
                        warn!("appsec: unable to encode WAF event: {e}\n{event:?}");
                        continue;
                    }
                };
                self.events.push(enc);
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
        let proc = Processor::new(&config).expect("Should not fail");
        match proc._diagnostics.get(b"ruleset_version") {
            None => panic!("Ruleset version should be present in diagnostics"),
            Some(version) => assert_ne!(version.to_str().expect("Should be a valid string"), ""),
        }
    }
}
