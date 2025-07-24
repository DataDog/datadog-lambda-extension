use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::appsec::sampler::Sampler;
use crate::appsec::{is_enabled, is_standalone, payload};
use crate::config::Config;

use bytes::Bytes;
use libddwaf::object::{WafMap, WafObject, WafOwned};
use libddwaf::{Builder, Config as WAFConfig, Context, Handle, RunResult, waf_map};
use tracing::{debug, info, warn};
/// The App & API Protection processor.
///
/// It is used to try to identify invoke requests that are supported, extract the relevant data from
/// the request payload, and evaluate in-app WAF rules against that data.
pub struct Processor {
    handle: Handle,
    waf_timeout: Duration,
    api_sec_sampler: Mutex<Sampler>,
}
impl Processor {
    /// Creates a new [`Processor`] instance using the provided [`Config`].
    ///
    /// # Errors
    /// - If [`Config::serverless_appsec_enabled`] is `false`;
    /// - If the [`Config::appsec_rules`] points to a non-existent file;
    /// - If the [`Config::appsec_rules`] points to a file that is not a valid JSON-encoded ruleset;
    /// - If the in-app WAF fails to initialize, integrate the ruleset, or build the WAF instance.
    pub fn new(config: &Config) -> Result<Self, Error> {
        if !is_enabled(config) {
            return Err(Error::FeatureDisabled);
        }
        debug!("Starting ASM processor");

        if is_standalone() {
            info!(
                "Starting ASM in standalone mode. APM tracing will be disabled for this service."
            );
        }

        let Some(mut builder) = Builder::new(&WAFConfig::default()) else {
            return Err(Error::BuilderCreationFailed);
        };

        let rules = Self::get_rules(config)?;
        let mut diagnostics = WafOwned::<WafMap>::default();
        if !builder.add_or_update_config("rules", &rules, Some(&mut diagnostics)) {
            return Err(Error::RulesetAdditionFailed(diagnostics));
        }

        let Some(handle) = builder.build() else {
            return Err(Error::WafCreationFailed);
        };

        if let Some(version) = diagnostics.get(b"ruleset_version").and_then(|o| o.to_str()) {
            debug!("appsec: loaded ruleset vesion: {version}");
        }

        Ok(Self {
            handle,
            waf_timeout: config.appsec_waf_timeout,
            api_sec_sampler: Mutex::new(Sampler::with_interval(config.api_security_sample_delay)),
        })
    }

    /// Process the `/runtime/invocation/next` payload, which is sent to Lambda to request an
    /// invocation event.
    #[must_use]
    pub async fn process_invocation_next(&self, body: &Bytes) -> Option<AppSecContext> {
        let (address_data, request_type) = payload::extract_request_address_data(body).await?;

        let mut context = AppSecContext {
            request_type,
            waf_context: Arc::new(Mutex::new(self.handle.new_context())),
            waf_timeout: self.waf_timeout,
            waf_duration: Duration::ZERO,
            timeouts: 0,
            keep: false,
            attributes: HashMap::new(),
            tags_always: HashMap::new(),
            tags_on_event: HashMap::new(),
            events: Vec::new(),
        };

        context
            .tags_always
            .insert("_dd.origin".to_string(), "appsec".to_string());

        context.absorb_data(
            address_data,
            None, // API Security samplingdecision is taken when the response is processed
        );
        Some(context)
    }

    /// Process the `/runtime/invocation/<request_id>/response>` payload, which is sent to Lambda
    /// after the invocation has run to completion, to provide the result of the invocation.
    pub async fn process_invocation_response(&self, context: &mut AppSecContext, body: &Bytes) {
        let address_data = payload::extract_response_address_data(context.request_type, body)
            .await
            .unwrap_or(payload::HttpData::Response {
                status_code: None,
                headers: None,
                body: None,
            });

        let mut api_sec_sampler = self
            .api_sec_sampler
            .lock()
            .inspect_err(|e| warn!("appsec: API security sampler mutex was poisoned: {e}"))
            .ok();
        context.absorb_data(address_data, api_sec_sampler.as_deref_mut());
    }

    /// Parses the App & API Protection ruleset from the provided [Config], falling back to the
    /// default built-in ruleset if the [Config] has [None].
    fn get_rules(config: &Config) -> Result<WafMap, Error> {
        // Default on recommended rules
        match &config.appsec_rules {
            None => {
                let default_rules = include_bytes!("rules.json");
                Ok(serde_json::from_slice(default_rules).map_err(Error::RulesetParseError)?)
            }
            Some(path) => {
                let rules = std::fs::File::open(path).map_err(|e| Error::RulesetFileError {
                    path: path.clone(),
                    cause: e,
                })?;
                Ok(serde_json::from_reader(rules).map_err(Error::RulesetParseError)?)
            }
        }
    }
}
impl std::fmt::Debug for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(Processor))
            .field("waf_timeout", &self.waf_timeout)
            .finish_non_exhaustive()
    }
}

/// Errors that can occur when calling [`Processor::new`].
#[derive(Debug)]
pub enum Error {
    FeatureDisabled,
    BuilderCreationFailed,
    RulesetFileError { path: String, cause: std::io::Error },
    RulesetParseError(serde_json::Error),
    RulesetAdditionFailed(libddwaf::object::WafOwned<WafMap>),
    WafCreationFailed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FeatureDisabled => write!(f, "appsec: feature disabled"),
            Self::BuilderCreationFailed => write!(f, "appsec: failed to create WAF builder"),
            Self::RulesetFileError { path, cause } => {
                write!(f, "appsec: failed to open ruleset file {path:#}: {cause}")
            }
            Self::RulesetParseError(e) => write!(f, "appsec: failed to parse ruleset: {e}"),
            Self::RulesetAdditionFailed(diags) => {
                write!(
                    f,
                    "appsec: failed to add ruleset to the WAF builder: {diags:?}"
                )
            }
            Self::WafCreationFailed => write!(f, "appsec: failed to build WAF instance"),
        }
    }
}
impl std::error::Error for Error {}

/// Request headers that are always collected as long as AAP is enabled.
///
/// This list should contain lowercase-normalized header names.
///
/// See: <https://datadoghq.atlassian.net/wiki/spaces/SAAL/pages/2186870984/HTTP+header+collection>.
const REQUEST_HEADERS_ON_EVENT: &[&str] = &[
    // IP address releated headers
    "x-forwarded-for",
    "x-real-ip",
    "true-client-ip",
    "x-client-ip",
    "x-forwarded",
    "forwarded-for",
    "x-cluster-client-ip",
    "fastly-client-ip",
    "cf-connecting-ip",
    "cf-connecting-ipv6",
    "forwarded",
    "via",
    // Message body information
    "content-length",
    "content-encoding",
    "content-language",
    // Host request context
    "host",
    // Content negotiation
    "accept-encoding",
    "accept-language",
];
/// Request headers that are collected only if AAP is enabled, and security activity has been
/// detected.
///
/// This list should contain lowercase-normalized header names.
///
/// See: <https://datadoghq.atlassian.net/wiki/spaces/SAAL/pages/2186870984/HTTP+header+collection>.
const REQUEST_HEADERS_ALWAYS: &[&str] = &[
    // Message body information
    "content-type",
    // Client user agent
    "user-agent",
    // Content negotiation
    "accept",
    // AWS WAF logs to traces (RFC 0996)
    "x-amzn-trace-id",
    // WAF Integration - Identify Requests (RFC 0992)
    "cloudfront-viewer-ja3-fingerprint",
    "cf-ray",
    "x-cloud-trace-context",
    "x-appgw-trace-id",
    "x-sigsci-requestid",
    "x-sigsci-tags",
    "akamai-user-risk",
];
/// Response headers that are always collected as long as AAP is enabled.
///
/// See: <https://datadoghq.atlassian.net/wiki/spaces/SAAL/pages/2186870984/HTTP+header+collection>.
const RESPONSE_HEADERS_ALWAYS: &[&str] = &[
    // Message body information
    "content-length",
    "content-type",
    "content-encoding",
    "content-language",
];

/// The WAF context for a single invocation.
///
/// This is used to process both the request & response of a given invocation.
#[derive(Clone)]
pub struct AppSecContext {
    request_type: payload::RequestType,
    // This must be clone-able due to how the request contexts are handled, so we have to wrap the
    // WAF context in an Arc-Mutex.
    waf_context: Arc<Mutex<Context>>,
    waf_timeout: Duration,
    pub waf_duration: Duration,
    pub timeouts: u32,
    pub keep: bool,
    attributes: HashMap<String, String>,
    /// The trace tags that are added to the trace unconditionally.
    tags_always: HashMap<String, String>,
    /// The trace tags that are added to the trace ONLY if there is a security event.
    tags_on_event: HashMap<String, String>,
    pub events: Vec<String>,
}
impl AppSecContext {
    /// Returns the list of trace tags to add to the trace.
    pub fn tags(&self) -> impl Iterator<Item = (&str, &str)> {
        let next: Box<dyn Iterator<Item = (&String, &String)>> = if self.events.is_empty() {
            Box::new(std::iter::empty())
        } else {
            Box::new(self.tags_on_event.iter())
        };
        self.attributes
            .iter()
            .chain(self.tags_always.iter())
            .chain(next)
            .map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Evaluates the appsec rules against the provided request data, and creates any relevant
    /// attributes from it.
    fn absorb_data(
        &mut self,
        address_data: payload::HttpData,
        api_sec_sampler: Option<&mut Sampler>,
    ) {
        let timeout = self.remaining_time_budget();
        if timeout == Duration::ZERO {
            warn!(
                "appsec: WAF timeout already reached ({waf:?}), not evaluating request with {address_data:?}",
                waf = &self.waf_duration,
            );
            return;
        }

        let recording_start = Instant::now();
        self.record_tags(&address_data);
        let recording_duration = recording_start.elapsed();
        debug!("appsec: processing tags took {recording_duration:?}");

        let extras = if let Some(sampler) = api_sec_sampler {
            let method = self
                .tags_always
                .get("http.method")
                .map_or("", String::as_str);
            let route = self
                .tags_on_event
                .get("http.route")
                .or_else(|| self.tags_always.get("http.url"))
                .map_or("", String::as_str);
            let status_code = self
                .tags_always
                .get("http.status_code")
                .map_or("", String::as_str);

            if sampler.decision_for(method, route, status_code) {
                vec![(
                    "waf.context.processor",
                    waf_map!(("extract-schema", true)).into(),
                )]
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        let encoding_start = Instant::now();
        let address_data = address_data.into_waf_map(extras);
        let encoding_duration = encoding_start.elapsed();
        debug!("appsec: encoding address data took {encoding_duration:?}");

        self.run(address_data, timeout);
    }

    /// Records the tags from the provided [`payload::HttpData`] into the [`Self::tags_always`] and
    /// [`Self::tags_on_event`] maps.
    fn record_tags(&mut self, address_data: &payload::HttpData) {
        match address_data {
            payload::HttpData::Request {
                raw_uri,
                method,
                route,
                client_ip,
                headers,
                ..
            } => {
                if let Some(uri) = raw_uri {
                    self.tags_always
                        .entry("http.url".to_string())
                        .or_insert(uri.clone());
                }
                if let Some(method) = method {
                    self.tags_always
                        .entry("http.method".to_string())
                        .or_insert(method.clone());
                }
                if let Some(route) = route {
                    self.tags_on_event
                        .entry("http.route".to_string())
                        .or_insert(route.clone());
                }
                if let Some(headers) = headers {
                    for name in REQUEST_HEADERS_ALWAYS {
                        let Some(values) = headers.get(*name) else {
                            continue;
                        };
                        self.tags_always
                            .entry(format!("http.request.headers.{name}"))
                            .or_insert(values.join(","));
                    }
                    for name in REQUEST_HEADERS_ON_EVENT {
                        let Some(values) = headers.get(*name) else {
                            continue;
                        };
                        self.tags_on_event
                            .entry(format!("http.request.headers.{name}"))
                            .or_insert(values.join(","));
                    }
                }
                if let Some(client_ip) = client_ip {
                    self.tags_on_event
                        .entry("network.client.ip".to_string())
                        .or_insert(client_ip.clone());
                }
            }
            payload::HttpData::Response {
                status_code,
                headers,
                ..
            } => {
                if let Some(status_code) = status_code {
                    self.tags_always
                        .entry("http.status_code".to_string())
                        .or_insert(status_code.to_string());
                }
                if let Some(headers) = headers {
                    for name in RESPONSE_HEADERS_ALWAYS {
                        let Some(values) = headers.get(*name) else {
                            continue;
                        };
                        self.tags_always
                            .entry(format!("http.response.headers.{name}"))
                            .or_insert(values.join(","));
                    }
                }
            }
        }
    }

    /// Evaluates the in-app WAF rules against the provided address data.
    fn run(&mut self, address_data: WafMap, timeout: Duration) {
        let mut waf_context = match self.waf_context.lock() {
            Ok(waf_context) => waf_context,
            Err(e) => {
                warn!("appsec: failed to lock WAF context: {e}");
                return;
            }
        };
        let result = match waf_context.run(Some(address_data), None, timeout) {
            Ok(RunResult::Match(result) | RunResult::NoMatch(result)) => result,
            Err(e) => {
                warn!("appsec: failed to evalute in-app WAF rules against request: {e}");
                return;
            }
        };

        self.waf_duration += result.duration();
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
                    let attr: &WafObject = attr; // Forcing deref type conversion
                    match serde_json::to_string(attr) {
                        Ok(value) => value,
                        Err(e) => {
                            warn!("appsec: unable to encode WAF attribute to JSON: {e}\n{attr:?}");
                            continue;
                        }
                    }
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

    /// Returns the time remaining to process WAF rules.
    const fn remaining_time_budget(&self) -> Duration {
        self.waf_timeout.saturating_add(self.waf_duration)
    }
}
impl std::fmt::Debug for AppSecContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(AppSecContext))
            .field("request_type", &self.request_type)
            .field("waf_timeout", &self.waf_timeout)
            .field("duration", &self.waf_duration)
            .field("timeouts", &self.timeouts)
            .field("keep", &self.keep)
            .field("attributes", &self.attributes)
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}
impl std::cmp::PartialEq for AppSecContext {
    fn eq(&self, other: &Self) -> bool {
        self.request_type == other.request_type
            && self.waf_timeout == other.waf_timeout
            && self.waf_duration == other.waf_duration
            && self.timeouts == other.timeouts
            && self.keep == other.keep
            && self.attributes == other.attributes
            && self.events == other.events
    }
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::config::Config;

    use super::*;

    use serde_json::json;

    #[test]
    fn test_new_with_default_config() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let _ = Processor::new(&config).expect("Should not fail");
    }

    #[test]
    fn test_new_disabled() {
        let config = Config {
            serverless_appsec_enabled: false, // Explicitly testing this condition
            ..Config::default()
        };
        assert!(matches!(
            Processor::new(&config),
            Err(Error::FeatureDisabled)
        ));
    }

    #[test]
    fn test_new_with_invalid_config() {
        let tmp = tempfile::NamedTempFile::new().expect("Failed to create tempfile");

        let config = Config {
            serverless_appsec_enabled: true,
            appsec_rules: Some(
                tmp.path()
                    .to_str()
                    .expect("Failed to get tempfile path")
                    .to_string(),
            ),
            ..Config::default()
        };
        assert!(matches!(
            Processor::new(&config),
            Err(Error::RulesetParseError(_))
        ));
    }

    #[test]
    fn test_new_with_no_rules_or_processors() {
        let mut tmp = tempfile::NamedTempFile::new().expect("Failed to create tempfile");
        tmp.write_all(
            br#"{
                "version": "2.2",
                "metadata":{
                    "ruleset_version": "0.0.0-blank"
                },
                "scanners":[{
                    "id": "406f8606-52c4-4663-8db9-df70f9e8766c",
                    "name": "ZIP Code",
                    "key": {
                        "operator": "match_regex",
                        "parameters": {
                            "regex": "\\b(?:zip|postal)\\b",
                            "options": {
                                "case_sensitive": false,
                                "min_length": 3
                            }
                        }
                    },
                    "value": {
                        "operator": "match_regex",
                        "parameters": {
                            "regex": "^[0-9]{5}(?:-[0-9]{4})?$",
                            "options": {
                                "case_sensitive": true,
                                "min_length": 5
                            }
                        }
                    },
                    "tags": {
                        "type": "zipcode",
                        "category": "address"
                    }
                }]
            }"#,
        )
        .expect("Failed to write to temp file");
        tmp.flush().expect("Failed to flush temp file");

        let config = Config {
            serverless_appsec_enabled: true,
            appsec_rules: Some(
                tmp.path()
                    .to_str()
                    .expect("Failed to get tempfile path")
                    .to_string(),
            ),
            ..Config::default()
        };
        let result = Processor::new(&config);
        assert!(
            matches!(
                result,
                Err(Error::WafCreationFailed), // There is no rule nor processor in the ruleset
            ),
            concat!(
                "should have failed with ",
                stringify!(Error::WafCreationFailed),
                " but was {:?}"
            ),
            result
        );
    }

    #[test]
    fn test_new_with_inexistent_ruleset_file() {
        let config = Config {
            serverless_appsec_enabled: true,
            appsec_rules: Some("/definitely/not/a/file/that/exists".to_string()),
            ..Config::default()
        };
        assert!(matches!(
            Processor::new(&config),
            Err(Error::RulesetFileError { .. })
        ));
    }

    #[tokio::test]
    async fn test_new_timed_out() {
        let config = Config {
            serverless_appsec_enabled: true,
            appsec_waf_timeout: Duration::ZERO, // Immediate timeout!
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");
        let context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/api_gateway_proxy_event.json"
            )))
            .await
            .expect("context should be Some");
        let tags = context.tags().collect::<HashMap<_, _>>();
        assert_eq!(tags, HashMap::from([("_dd.origin", "appsec")]));
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_api_gateway_v1() {
        let config = Config {
            serverless_appsec_enabled: true,
            appsec_waf_timeout: Duration::from_secs(3600), // Avoids falkes on slower CI hardware
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/api_gateway_proxy_event.appsec_event.json"
            )))
            .await
            .expect("an AppSec context should have been created");
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);
        // Duration will be greater than zero due to WAF processing
        assert!(context.waf_duration > Duration::ZERO);
        assert_eq!(context.timeouts, 0);
        assert!(context.keep);
        assert!(!context.events.is_empty(), "should have at least one event");
        assert!(
            context.events.iter().any(|e| e.contains("Arachni/v2")),
            "at least one of the events should mention Arachni/v2"
        );
        assert_eq!(
            context.tags().collect::<HashMap<_, _>>(),
            HashMap::from([
                // Fingerprints added by the WAF
                (
                    "_dd.appsec.fp.http.header",
                    "hdr-0010100011-40b52535-12-d7bf5e5b"
                ),
                ("_dd.appsec.fp.http.network", "net-2-1000000000"),
                // Origin tag
                ("_dd.origin", "appsec"),
                // Extracted HTTP request information (complete, there is a security event here)
                ("http.method", "POST"),
                (
                    "http.request.headers.accept-encoding",
                    "gzip, deflate, sdch"
                ),
                ("http.request.headers.accept-language", "en-US,en;q=0.8"),
                (
                    "http.request.headers.accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8"
                ),
                (
                    "http.request.headers.host",
                    "0123456789.execute-api.us-east-1.amazonaws.com"
                ),
                ("http.request.headers.user-agent", "Arachni/v2"),
                (
                    "http.request.headers.via",
                    "1.1 08f323deadbeefa7af34d5feb414ce27.cloudfront.net (CloudFront)"
                ),
                (
                    "http.request.headers.x-forwarded-for",
                    "127.0.0.1, 127.0.0.2"
                ),
                ("http.route", "/{proxy+}"),
                ("http.url", "/path/to/resource"),
                ("network.client.ip", "127.0.0.1"),
            ])
        );
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_api_gateway_v2() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/api_gateway_http_event.json"
            )))
            .await
            .expect("an AppSec context should have been created");
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV2Http);
        assert!(
            context.events.is_empty(),
            "should not have produced any AppSec event"
        );
        assert_eq!(
            context.tags_always.get("_dd.origin"),
            Some(&"appsec".to_string())
        );
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_unsupported_payload() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/sns_event.json"
            )))
            .await;

        // SNS events are not supported, so should return None
        assert!(context.is_none());
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_invalid_json() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{"invalid": json}"#;

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        // Invalid JSON should return None
        assert!(context.is_none());
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_empty_payload() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = "";

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        // Empty payload should return None
        assert!(context.is_none());
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_api_gateway_v1() {
        let config = Config {
            serverless_appsec_enabled: true,
            appsec_waf_timeout: Duration::from_secs(3600), // Avoids falkes on slower CI hardware
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let mut context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/api_gateway_proxy_event.json"
            )))
            .await
            .expect("Should create context");

        // Now test the response processing
        let response_payload = r#"{
            "statusCode": 200,
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "{\"response\":\"success\"}",
            "isBase64Encoded": false
        }"#;

        let response_bytes = Bytes::from(response_payload);

        // This should not panic and should complete successfully
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state after response processing (unchanged)
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);
        assert_eq!(context.timeouts, 0);
        assert!(context.events.is_empty());
        assert_eq!(
            context
                .tags()
                .filter(|(k, _)| !k.starts_with("_dd.appsec.s."))
                .collect::<HashMap<_, _>>(),
            HashMap::from([
                // Fingerprints added by the WAF
                (
                    "_dd.appsec.fp.http.header",
                    "hdr-0010100011-8a1b5aba-12-d7bf5e5b"
                ),
                ("_dd.appsec.fp.http.network", "net-2-1000000000"),
                // Origin tag
                ("_dd.origin", "appsec"),
                // Extracted HTTP request information (shallow, no security event here)
                ("http.method", "POST"),
                (
                    "http.request.headers.accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8"
                ),
                (
                    "http.request.headers.user-agent",
                    "Custom User Agent String"
                ),
                ("http.response.headers.content-type", "application/json"),
                ("http.status_code", "200"),
                ("http.url", "/path/to/resource"),
            ])
        );

        // Schemas extracted by the WAF (encoded order is not necessarily deterministic, so we compare after parsing back)
        assert_eq!(
            context
                .tags()
                .filter(|(k, _)| k.starts_with("_dd.appsec.s."))
                .map(|(k, v)| (k, serde_json::from_str(v).expect("should be valid JSON")))
                .collect::<HashMap<_, serde_json::Value>>(),
            HashMap::from([
                ("_dd.appsec.s.res.body", json!([{"response":[8]}])),
                (
                    "_dd.appsec.s.res.headers",
                    json!([{"content-type":[[[8]],{"len":1}]}])
                ),
                ("_dd.appsec.s.req.body", json!([{"test":[8]}])),
                (
                    "_dd.appsec.s.req.headers",
                    json!(
                        [{"host":[[[8]],{"len":1}],"accept-encoding":[[[8]],{"len":1}],"x-amz-cf-id":[[[8]],{"len":1}],"via":[[[8]],{"len":1}],"cloudfront-is-desktop-viewer":[[[8]],{"len":1}],"accept-language":[[[8]],{"len":1}],"x-forwarded-port":[[[8]],{"len":1}],"cloudfront-viewer-country":[[[8]],{"len":1}],"cloudfront-is-mobile-viewer":[[[8]],{"len":1}],"cache-control":[[[8]],{"len":1}],"cloudfront-forwarded-proto":[[[8]],{"len":1}],"x-forwarded-proto":[[[8]],{"len":1}],"cloudfront-is-tablet-viewer":[[[8]],{"len":1}],"upgrade-insecure-requests":[[[8]],{"len":1}],"user-agent":[[[8]],{"len":1}],"cloudfront-is-smarttv-viewer":[[[8]],{"len":1}],"x-forwarded-for":[[[8]],{"len":1}],"accept":[[[8]],{"len":1}]}]
                    )
                ),
                ("_dd.appsec.s.req.params", json!([{"proxy":[8]}])),
                ("_dd.appsec.s.req.query", json!([{"foo":[[[8]],{"len":1}]}])),
            ])
        );
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_api_gateway_v2() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let mut context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/api_gateway_http_event.json"
            )))
            .await
            .expect("Should create context");

        // Now test the response processing
        let response_payload = r#"{
            "statusCode": 200,
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "{\"response\":\"success\"}",
            "isBase64Encoded": false
        }"#;

        let response_bytes = Bytes::from(response_payload);

        // This should not panic and should complete successfully
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state after response processing (unchanged)
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV2Http);
        //TODO(romain.marcadier): Verify additional side-effects
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_invalid_response() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let mut context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/api_gateway_proxy_event.json"
            )))
            .await
            .expect("Should create context");

        // Now test the response processing with invalid JSON
        let response_payload = r#"{"invalid": json}"#;

        let response_bytes = Bytes::from(response_payload);

        // This should not panic even with invalid response JSON
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state is still valid (unchanged)
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);

        // Schemas extracted by the WAF (encoded order is not necessarily deterministic, so we compare after parsing back)
        // We cannot have observed the response body (it's not valid), but we have been able to observe the request...
        assert_eq!(
            context
                .tags()
                .filter(|(k, _)| k.starts_with("_dd.appsec.s."))
                .map(|(k, v)| (k, serde_json::from_str(v).expect("should be valid JSON")))
                .collect::<HashMap<_, serde_json::Value>>(),
            HashMap::from([
                ("_dd.appsec.s.req.body", json!([{"test":[8]}])),
                (
                    "_dd.appsec.s.req.headers",
                    json!(
                        [{"host":[[[8]],{"len":1}],"accept-encoding":[[[8]],{"len":1}],"x-amz-cf-id":[[[8]],{"len":1}],"via":[[[8]],{"len":1}],"cloudfront-is-desktop-viewer":[[[8]],{"len":1}],"accept-language":[[[8]],{"len":1}],"x-forwarded-port":[[[8]],{"len":1}],"cloudfront-viewer-country":[[[8]],{"len":1}],"cloudfront-is-mobile-viewer":[[[8]],{"len":1}],"cache-control":[[[8]],{"len":1}],"cloudfront-forwarded-proto":[[[8]],{"len":1}],"x-forwarded-proto":[[[8]],{"len":1}],"cloudfront-is-tablet-viewer":[[[8]],{"len":1}],"upgrade-insecure-requests":[[[8]],{"len":1}],"user-agent":[[[8]],{"len":1}],"cloudfront-is-smarttv-viewer":[[[8]],{"len":1}],"x-forwarded-for":[[[8]],{"len":1}],"accept":[[[8]],{"len":1}]}]
                    )
                ),
                ("_dd.appsec.s.req.params", json!([{"proxy":[8]}])),
                ("_dd.appsec.s.req.query", json!([{"foo":[[[8]],{"len":1}]}])),
            ])
        );
    }

    #[tokio::test]
    async fn test_process_invocation_response_with_empty_response() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // First create a context with a request
        let mut context = processor
            .process_invocation_next(&Bytes::from(include_str!(
                "../../tests/payloads/api_gateway_proxy_event.json"
            )))
            .await
            .expect("Should create context");

        // Now test the response processing with empty response
        let response_payload = "";

        let response_bytes = Bytes::from(response_payload);

        // This should not panic even with empty response
        processor
            .process_invocation_response(&mut context, &response_bytes)
            .await;

        // Verify context state is still valid (unchanged)
        assert_eq!(context.request_type, payload::RequestType::APIGatewayV1);
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_custom_authorizer_token_full() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "type": "TOKEN",
            "methodArn": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/request",
            "authorizationToken": "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        // Token style authorizers may or may not be supported depending on available data
        if let Some(context) = context {
            assert_eq!(
                context.request_type,
                payload::RequestType::APIGatewayLambdaAuthorizerToken
            );
        }
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_custom_authorizer_token_minimal() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "type": "TOKEN",
            "methodArn": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/request",
            "authorizationToken": "allow"
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor.process_invocation_next(&bytes).await;

        // Token style authorizers may or may not be supported depending on available data
        if let Some(context) = context {
            assert_eq!(
                context.request_type,
                payload::RequestType::APIGatewayLambdaAuthorizerToken
            );
        }
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_custom_authorizer_request_full() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "type": "REQUEST",
            "methodArn": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/request",
            "resource": "/request",
            "path": "/request",
            "httpMethod": "GET",
            "headers": {
                "Accept": "*/*",
                "Authorization": "Bearer token123",
                "Content-Type": "application/json",
                "Host": "example.execute-api.us-east-1.amazonaws.com",
                "User-Agent": "Mozilla/5.0",
                "X-Forwarded-For": "192.168.1.1, 10.0.0.1"
            },
            "multiValueHeaders": {
                "Accept": ["*/*"],
                "Authorization": ["Bearer token123"],
                "Content-Type": ["application/json"],
                "Host": ["example.execute-api.us-east-1.amazonaws.com"],
                "User-Agent": ["Mozilla/5.0"],
                "X-Forwarded-For": ["192.168.1.1, 10.0.0.1"]
            },
            "queryStringParameters": {},
            "multiValueQueryStringParameters": {},
            "pathParameters": {},
            "stageVariables": {},
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/request",
                "httpMethod": "GET",
                "extendedRequestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "requestTime": "09/Apr/2015:12:34:56 +0000",
                "path": "/test/request",
                "accountId": "123456789012",
                "protocol": "HTTP/1.1",
                "stage": "test",
                "domainPrefix": "example",
                "requestTimeEpoch": 1428582896000,
                "requestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "identity": {
                    "cognitoIdentityPoolId": null,
                    "accountId": null,
                    "cognitoIdentityId": null,
                    "caller": null,
                    "accessKey": null,
                    "sourceIp": "192.168.1.1",
                    "cognitoAuthenticationType": null,
                    "cognitoAuthenticationProvider": null,
                    "userArn": null,
                    "userAgent": "Mozilla/5.0",
                    "user": null
                },
                "domainName": "example.execute-api.us-east-1.amazonaws.com",
                "apiId": "abcdef123"
            }
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor
            .process_invocation_next(&bytes)
            .await
            .expect("Should create context for request style authorizer");

        assert_eq!(
            context.request_type,
            payload::RequestType::APIGatewayLambdaAuthorizerRequest
        );

        // Verify that some basic tags are present
        let tags: HashMap<_, _> = context.tags().collect();

        assert_eq!(tags.get("_dd.origin"), Some(&"appsec"));
        assert_eq!(tags.get("http.method"), Some(&"GET"));
        assert_eq!(tags.get("http.url"), Some(&"/request"));
        assert_eq!(
            tags.get("http.request.headers.user-agent"),
            Some(&"Mozilla/5.0")
        );
        assert_eq!(tags.get("network.client.ip"), None); // Only collected if there is a security event
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_custom_authorizer_request_with_body() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "type": "REQUEST",
            "methodArn": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/POST/request",
            "resource": "/request",
            "path": "/request",
            "httpMethod": "POST",
            "headers": {
                "Accept": "application/json",
                "Content-Type": "application/json",
                "Host": "example.execute-api.us-east-1.amazonaws.com"
            },
            "multiValueHeaders": {
                "Accept": ["application/json"],
                "Content-Type": ["application/json"],
                "Host": ["example.execute-api.us-east-1.amazonaws.com"]
            },
            "queryStringParameters": {},
            "multiValueQueryStringParameters": {},
            "pathParameters": {},
            "stageVariables": {},
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/request",
                "httpMethod": "POST",
                "extendedRequestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "requestTime": "09/Apr/2015:12:34:56 +0000",
                "path": "/test/request",
                "accountId": "123456789012",
                "protocol": "HTTP/1.1",
                "stage": "test",
                "domainPrefix": "example",
                "requestTimeEpoch": 1428582896000,
                "requestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "identity": {
                    "sourceIp": "10.0.0.1",
                    "userAgent": "curl/7.64.1"
                },
                "domainName": "example.execute-api.us-east-1.amazonaws.com",
                "apiId": "abcdef123"
            },
            "body": "{\"key\":\"value\"}",
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor
            .process_invocation_next(&bytes)
            .await
            .expect("Should create context for request style authorizer with body");

        assert_eq!(
            context.request_type,
            payload::RequestType::APIGatewayLambdaAuthorizerRequest
        );

        // Verify that some basic tags are present
        let tags: HashMap<_, _> = context.tags().collect();

        assert_eq!(tags.get("_dd.origin"), Some(&"appsec"));
        assert_eq!(tags.get("http.method"), Some(&"POST"));
        assert_eq!(tags.get("http.url"), Some(&"/request"));
        assert_eq!(
            tags.get("http.request.headers.content-type"),
            Some(&"application/json")
        );
        assert_eq!(tags.get("network.client.ip"), None); // Only collected if there is a security event
    }

    #[tokio::test]
    async fn test_process_invocation_next_with_custom_authorizer_request_minimal() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        let payload = r#"{
            "type": "REQUEST",
            "methodArn": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/request",
            "resource": "/request",
            "path": "/request",
            "httpMethod": "GET",
            "headers": {},
            "multiValueHeaders": {},
            "queryStringParameters": {},
            "multiValueQueryStringParameters": {},
            "pathParameters": {},
            "stageVariables": {},
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/request",
                "httpMethod": "GET",
                "path": "/test/request",
                "accountId": "123456789012",
                "protocol": "HTTP/1.1",
                "stage": "test",
                "requestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "identity": {
                    "sourceIp": "127.0.0.1"
                },
                "domainName": "example.execute-api.us-east-1.amazonaws.com",
                "apiId": "abcdef123"
            }
        }"#;

        let bytes = Bytes::from(payload);
        let context = processor
            .process_invocation_next(&bytes)
            .await
            .expect("Should create context for minimal request style authorizer");

        assert_eq!(
            context.request_type,
            payload::RequestType::APIGatewayLambdaAuthorizerRequest
        );

        // Verify that basic tags are present even with minimal payload
        let tags: HashMap<_, _> = context.tags().collect();

        assert_eq!(tags.get("_dd.origin"), Some(&"appsec"));
        assert_eq!(tags.get("http.method"), Some(&"GET"));
        assert_eq!(tags.get("http.url"), Some(&"/request"));
        assert_eq!(tags.get("network.client.ip"), None); // Only collected if there is a security event
    }

    #[tokio::test]
    async fn test_custom_authorizer_payload_extraction_debug() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let processor = Processor::new(&config).expect("Should not fail");

        // Test TOKEN style payload
        let token_payload = r#"{
            "type": "TOKEN",
            "methodArn": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/request",
            "authorizationToken": "Bearer test-token"
        }"#;

        let bytes = Bytes::from(token_payload);
        let context = processor.process_invocation_next(&bytes).await;

        if let Some(context) = context {
            assert_eq!(
                context.request_type,
                payload::RequestType::APIGatewayLambdaAuthorizerToken
            );
        }

        // Test REQUEST style payload
        let request_payload = r#"{
            "type": "REQUEST",
            "methodArn": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/request",
            "resource": "/request",
            "path": "/request",
            "httpMethod": "GET",
            "headers": {
                "Authorization": "Bearer test-token",
                "Host": "example.execute-api.us-east-1.amazonaws.com"
            },
            "multiValueHeaders": {
                "Authorization": ["Bearer test-token"],
                "Host": ["example.execute-api.us-east-1.amazonaws.com"]
            },
            "queryStringParameters": {},
            "multiValueQueryStringParameters": {},
            "pathParameters": {},
            "stageVariables": {},
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/request",
                "httpMethod": "GET",
                "path": "/test/request",
                "accountId": "123456789012",
                "protocol": "HTTP/1.1",
                "stage": "test",
                "requestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "identity": {
                    "sourceIp": "192.168.1.1"
                },
                "domainName": "example.execute-api.us-east-1.amazonaws.com",
                "apiId": "abcdef123"
            }
        }"#;

        let bytes = Bytes::from(request_payload);
        let context = processor
            .process_invocation_next(&bytes)
            .await
            .expect("Should create context for request style authorizer");

        assert_eq!(
            context.request_type,
            payload::RequestType::APIGatewayLambdaAuthorizerRequest
        );

        // Verify extracted data
        let tags: HashMap<_, _> = context.tags().collect();

        assert_eq!(tags.get("_dd.origin"), Some(&"appsec"));
        assert_eq!(tags.get("http.method"), Some(&"GET"));
        assert_eq!(tags.get("http.url"), Some(&"/request"));
        assert_eq!(tags.get("network.client.ip"), None); // Only collected if there is a security event
    }
}
