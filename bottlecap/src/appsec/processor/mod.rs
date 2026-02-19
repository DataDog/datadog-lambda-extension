use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Read;
use std::num::NonZero;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cookie::Cookie;
use itertools::Itertools;
use libdd_trace_protobuf::pb::Span;
use libddwaf::object::{WafMap, WafOwned};
use libddwaf::{Builder, Config as WafConfig, Handle};
use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::appsec::processor::context::Context;
use crate::appsec::processor::response::ExpectedResponseFormat;
use crate::appsec::{is_enabled, is_standalone};
use crate::config::Config;
use crate::lifecycle::invocation::triggers::IdentifiedTrigger;

mod apisec;
pub mod context;
pub mod response;
mod ruleset;

pub struct Processor {
    handle: Handle,
    ruleset_version: String,
    waf_timeout: Duration,
    api_sec_sampler: Option<Arc<Mutex<apisec::Sampler>>>, // Must be [`Arc`] so [`Processor`] can be [`Send`].
    context_buffer: VecDeque<Context>,
}
impl Processor {
    const CONTEXT_BUFFER_DEFAULT_CAPACITY: NonZero<usize> = NonZero::new(5).unwrap();

    /// Creates a new [`Processor`] instance using the provided [`Config`].
    ///
    /// # Errors
    /// - If [`Config::serverless_appsec_enabled`] is `false`;
    /// - If the [`Config::appsec_rules`] points to a non-existent file;
    /// - If the [`Config::appsec_rules`] points to a file that is not a valid JSON-encoded ruleset;
    /// - If the in-app WAF fails to initialize, integrate the ruleset, or build the WAF instance.
    pub fn new(cfg: &Config) -> Result<Self, Error> {
        Self::with_capacity(cfg, Self::CONTEXT_BUFFER_DEFAULT_CAPACITY)
    }

    /// Creates a new [`Processor`] instance using the provided [`Config`].
    ///
    /// # Errors
    /// - If [`Config::serverless_appsec_enabled`] is `false`;
    /// - If the [`Config::appsec_rules`] points to a non-existent file;
    /// - If the [`Config::appsec_rules`] points to a file that is not a valid JSON-encoded ruleset;
    /// - If the in-app WAF fails to initialize, integrate the ruleset, or build the WAF instance.
    pub fn with_capacity(cfg: &Config, capacity: NonZero<usize>) -> Result<Self, Error> {
        if !is_enabled(cfg) {
            return Err(Error::FeatureDisabled);
        }

        debug!("aap: starting App & API Protection processor");
        if is_standalone() {
            info!(
                "aap: starting App & API Protection in standalone mode. APM tracing will be disabled for this service."
            );
        }

        let Some(mut builder) = Builder::new(&WafConfig::default()) else {
            return Err(Error::WafBuilderCreationFailed);
        };

        let rules = Self::get_rules(cfg)?;
        let mut diagnostics = WafOwned::<WafMap>::default();
        if !builder.add_or_update_config("rules", &rules, Some(&mut diagnostics)) {
            return Err(Error::WafRulesetLoadingError(diagnostics));
        }
        let ruleset_version =
            if let Some(version) = diagnostics.get(b"ruleset_version").and_then(|o| o.to_str()) {
                debug!("aap: loaded ruleset version {version}");
                version.to_string()
            } else {
                String::new()
            };

        let Some(handle) = builder.build() else {
            return Err(Error::WafInitializationFailed);
        };

        Ok(Self {
            handle,
            ruleset_version,
            waf_timeout: cfg.appsec_waf_timeout,
            api_sec_sampler: if cfg.api_security_enabled {
                Some(Arc::new(Mutex::new(apisec::Sampler::with_interval(
                    cfg.api_security_sample_delay,
                ))))
            } else {
                None
            },
            context_buffer: VecDeque::with_capacity(capacity.get()),
        })
    }

    /// Process the intercepted payload for the next invocation.
    pub async fn process_invocation_next(&mut self, rid: &str, payload: &IdentifiedTrigger) {
        if payload.is_unknown() {
            return;
        }
        self.new_context(rid).await.run(payload);
    }

    /// Process the intercepted payload for the result of an invocation.
    pub async fn process_invocation_result(&mut self, rid: &str, payload: &Bytes) {
        // Taking the sampler first, as it implies a temporary immutable borrow...
        let api_sec_sampler = self.api_sec_sampler.as_ref().map(Arc::clone);
        let api_sec_sampler = if let Some(api_sec_sampler) = &api_sec_sampler {
            Some(api_sec_sampler.lock().await)
        } else {
            None
        };

        let Some(ctx) = self.get_context_mut(rid) else {
            // Nothing to do...
            return;
        };

        match ctx.expected_response_format.parse(payload.as_ref()) {
            Ok(Some(payload)) => {
                debug!("aap: successfully parsed response payload, evaluating ruleset...");
                ctx.run(payload.as_ref());
            }
            Ok(None) => debug!("aap: no response payload available"),
            Err(e) => debug!("aap: failed to parse invocation result payload: {e}"),
        }

        let (method, route, status_code) = ctx.endpoint_info();
        if api_sec_sampler
            .is_some_and(|mut sampler| sampler.decision_for(&method, &route, &status_code))
        {
            debug!(
                "aap: extracing API Security schema for request <{method}, {route}, {status_code}>"
            );
            ctx.extract_schemas();
        }

        // Finally, mark the response as having been seen.
        ctx.set_response_seen().await;
    }

    /// Returns the first `aws.lambda` span from the provided trace, if one
    /// exists.
    ///
    /// # Returns
    /// The span on which security information will be attached.
    pub fn service_entry_span_mut(trace: &mut [Span]) -> Option<&mut Span> {
        trace.iter_mut().find(|span| span.name == "aws.lambda")
    }

    /// Processes an intercepted [`Span`].
    ///
    /// # Returns
    /// Returns [`true`] the span can already be finalized and sent to the
    /// backend, meaning that if there was a security context, we have already
    /// had a chance to see the response for the corresponding span already.
    /// Otherwise, returns false, and an optional [`Context`] that can be used
    /// to defer some processing to when the response becomes available.
    pub fn process_span(&mut self, span: &mut Span) -> (bool, Option<&mut Context>) {
        let Some(rid) = span.meta.get("request_id").cloned() else {
            // Can't match this to a request ID, nothing to do...
            debug!(
                "aap | {} @ {} | no request_id found in span meta, nothing to do...",
                span.name, span.span_id
            );
            return (true, None);
        };
        let Some(ctx) = self.get_context(&rid) else {
            // Nothing to do...
            debug!(
                "aap | {} @ {} | no security context is associated with request {rid}, nothing to do...",
                span.name, span.span_id
            );
            return (true, None);
        };
        debug!(
            "aap | {} @ {} | processing span with request {rid}",
            span.name, span.span_id
        );
        ctx.process_span(span);

        // Span is finalized from a security standpoint if we've seen the
        // response for it already.
        if !ctx.is_pending_response() {
            debug!(
                "aap | {} @ {} | span is finalized, deleting context",
                span.name, span.span_id
            );
            self.delete_context(&rid);
            return (true, None);
        }

        // Fetch the context again here, as otherwise the borrow checker yells.
        (false, self.get_context_mut(&rid))
    }

    /// Parses the App & API Protection ruleset from the provided [`Config`], or
    /// the default built-in ruleset if the [`Config::appsec_rules`] field is
    /// [`None`].
    fn get_rules(cfg: &Config) -> Result<WafMap, Error> {
        if let Some(ref rules) = cfg.appsec_rules {
            let file = File::open(rules).map_err(|e| Error::AppsecRulesError(rules.clone(), e))?;
            serde_json::from_reader(file)
        } else {
            serde_json::from_reader(ruleset::default_recommended_ruleset())
        }
        .map_err(Error::WafRulesetParseError)
    }

    /// Creates a new [`Context`] for the given request ID, and tracks it in the
    /// context buffer.
    async fn new_context(&mut self, rid: &str) -> &mut Context {
        let dropped = if let Some(idx) = self.context_buffer.iter().position(|c| c.rid == rid) {
            // This request ID was already seen, remove it from the buffer...
            self.context_buffer.remove(idx)
        } else if self.context_buffer.len() == self.context_buffer.capacity() {
            // We're at capacity, remove the oldest context before proceeding...
            self.context_buffer.pop_front()
        } else {
            None
        };
        if let Some(mut ctx) = dropped {
            // Ensure any pending trace is flushed out before continuing...
            ctx.set_response_seen().await;
        }

        // Insert the new context at the back of the buffer.
        self.context_buffer.push_back(Context::new(
            rid.to_string(),
            &mut self.handle,
            &self.ruleset_version,
            self.waf_timeout,
        ));
        // Retrieve a mutable reference to it from the buffer.
        self.context_buffer
            .back_mut()
            .expect("should have at least one element")
    }

    /// Retrieves the [`Context`] associated with the given request ID, if there
    /// is one.
    fn get_context(&self, rid: &str) -> Option<&Context> {
        self.context_buffer.iter().find(|c| c.rid == rid)
    }

    /// Retrieves the [`Context`] associated with the given request ID, if there
    /// is one.
    fn get_context_mut(&mut self, rid: &str) -> Option<&mut Context> {
        self.context_buffer.iter_mut().find(|c| c.rid == rid)
    }

    /// Removes the [`Context`] associated with the given request ID from the
    /// buffer, if there is one.
    fn delete_context(&mut self, rid: &str) {
        if let Some(idx) = self.context_buffer.iter().position(|c| c.rid == rid) {
            self.context_buffer.remove(idx);
        }
    }
}
impl std::fmt::Debug for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Processor")
            .field("waf_timeout", &self.waf_timeout)
            .finish_non_exhaustive()
    }
}

/// Error conditions that can arise from calling [`Processor::new`].
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// The App & API Protection feature is not enabled
    #[error("aap: feature is not enabled")]
    FeatureDisabled,
    /// The WAF builder could not be created (unlikely)
    #[error("aap: WAF builder creation failed")]
    WafBuilderCreationFailed,
    /// The user-configured App & API Protection ruleset file could not be read
    #[error("aap: failed to open WAF rules file {0:#?}: {1}")]
    AppsecRulesError(String, std::io::Error),
    /// The App & API Protection ruleset could not be parsed from JSON
    #[error("aap: failed to parse WAF rulesset: {0}")]
    WafRulesetParseError(serde_json::Error),
    /// The App & API Protection ruleset could not be loaded into the WAF
    #[error("aap: failed to load configured WAF ruleset: {0:?}")]
    WafRulesetLoadingError(WafOwned<WafMap>),
    /// The WAF initialization produced a [`None`] handle (this happens when the
    /// configured ruleset contains no active rule nor processor)
    #[error(
        "aap: WAF initialization failed (is there any active rule or processor in the ruleset?)"
    )]
    WafInitializationFailed,
}

/// A trait representing the general payload of an invocation event. Most
/// fields are optional and default to [`None`] or empty [`HashMap`]s.
pub trait InvocationPayload {
    fn corresponding_response_format(&self) -> ExpectedResponseFormat;

    /// The raw URI of the request (query string excluded).
    fn raw_uri(&self) -> Option<String> {
        None
    }
    /// The HTTP method for the request (e.g. `GET`, `POST`, etc.).
    fn method(&self) -> Option<String> {
        None
    }
    /// The route for the request (e.g. `/api/v1/users/{id}`).
    fn route(&self) -> Option<String> {
        None
    }
    /// The Client IP for the request (e.g. `127.0.0.1`).
    fn client_ip(&self) -> Option<String> {
        None
    }
    /// The multi-value headers for the request, without the cookies.
    fn request_headers_no_cookies(&self) -> HashMap<String, Vec<String>> {
        HashMap::default()
    }
    /// The cookies for the request.
    fn request_cookies(&self) -> HashMap<String, Vec<String>> {
        HashMap::default()
    }
    /// The query string parameters for the request.
    fn query_params(&self) -> HashMap<String, Vec<String>> {
        HashMap::default()
    }
    /// The path parameters for the request.
    fn path_params(&self) -> HashMap<String, String> {
        HashMap::default()
    }
    /// The body payload of the request encoded as a [`WafObject`].
    fn request_body<'a>(&'a self) -> Option<Box<dyn Read + 'a>> {
        None
    }

    /// The status code of the response (e.g, 200, 404, etc...).
    fn response_status_code(&self) -> Option<i64> {
        None
    }
    /// The headers for the response, without the cookies.
    fn response_headers_no_cookies(&self) -> HashMap<String, Vec<String>> {
        HashMap::default()
    }
    /// The body payload of the response encoded as a [`WafObject`].
    fn response_body<'a>(&'a self) -> Option<Box<dyn Read + 'a>> {
        None
    }
}

impl InvocationPayload for IdentifiedTrigger {
    fn corresponding_response_format(&self) -> ExpectedResponseFormat {
        match self {
            Self::APIGatewayHttpEvent(_)
            | Self::APIGatewayRestEvent(_)
            | Self::ALBEvent(_)
            | Self::LambdaFunctionUrlEvent(_) => ExpectedResponseFormat::ApiGatewayResponse,
            Self::APIGatewayWebSocketEvent(_) => ExpectedResponseFormat::Raw,

            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => ExpectedResponseFormat::Unknown,
        }
    }

    fn raw_uri(&self) -> Option<String> {
        match self {
            Self::APIGatewayHttpEvent(t) => Some(format!(
                "{domain}{path}",
                domain = t.request_context.domain_name,
                path = t.request_context.http.path
            )),
            Self::APIGatewayRestEvent(t) => Some(format!(
                "{domain}{path}",
                domain = t.request_context.domain_name,
                path = t.request_context.path
            )),
            Self::APIGatewayWebSocketEvent(t) => Some(
                if t.request_context.stage.is_empty() || t.request_context.stage == "$default" {
                    format!(
                        "{domain}{path}",
                        domain = t.request_context.domain_name,
                        path = t.path.as_ref().map_or("", |s| s.as_str())
                    )
                } else {
                    format!(
                        "{domain}/${stage}{path}",
                        domain = t.request_context.domain_name,
                        stage = t.request_context.stage,
                        path = t.path.as_ref().map_or("", |s| s.as_str())
                    )
                },
            ),

            #[allow(clippy::match_same_arms)]
            Self::ALBEvent(_) => None,
            Self::LambdaFunctionUrlEvent(t) => Some(format!(
                "{domain}{path}",
                domain = t.request_context.domain_name,
                path = t.request_context.http.path
            )),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => None,
        }
    }
    fn method(&self) -> Option<String> {
        match self {
            Self::APIGatewayHttpEvent(t) => Some(t.request_context.http.method.clone()),
            Self::APIGatewayRestEvent(t) => Some(t.request_context.method.clone()),
            Self::APIGatewayWebSocketEvent(t) => t.request_context.http_method.clone(),
            Self::ALBEvent(t) => Some(t.http_method.clone()),
            Self::LambdaFunctionUrlEvent(t) => Some(t.request_context.http.method.clone()),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => None,
        }
    }
    fn route(&self) -> Option<String> {
        match self {
            Self::APIGatewayHttpEvent(t) => Some(t.route_key.clone()),
            Self::APIGatewayRestEvent(t) => Some(format!(
                "{method} {resource}",
                method = t.request_context.method,
                resource = t.request_context.resource_path
            )),
            Self::APIGatewayWebSocketEvent(t) => Some(t.request_context.route_key.clone()),
            Self::ALBEvent(t) => Some(format!(
                "{method} {path}",
                method = t.http_method,
                path = t.path.as_ref().map_or("", |s| s.as_str()),
            )),
            Self::LambdaFunctionUrlEvent(t) => Some(format!(
                "{method} {path}",
                method = t.request_context.http.method,
                path = t.request_context.http.path
            )),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => None,
        }
    }
    fn client_ip(&self) -> Option<String> {
        match self {
            Self::APIGatewayHttpEvent(t) => Some(t.request_context.http.source_ip.clone()),
            Self::APIGatewayRestEvent(t) => Some(t.request_context.identity.source_ip.clone()),
            Self::APIGatewayWebSocketEvent(t) => t.request_context.identity.source_ip.clone(),
            #[allow(clippy::match_same_arms)]
            Self::ALBEvent(_) => None, // TODO: Can we extract from the headers instead?
            Self::LambdaFunctionUrlEvent(t) => Some(t.request_context.http.source_ip.clone()),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => None,
        }
    }
    fn request_headers_no_cookies(&self) -> HashMap<String, Vec<String>> {
        fn cloned<'a, K: Clone + 'a, V: Clone + 'a>((k, v): (&'a K, &'a V)) -> (K, V) {
            (k.clone(), v.clone())
        }
        fn as_multi<K, V>((k, v): (K, V)) -> (K, Vec<V>) {
            (k, vec![v])
        }
        fn without_cookie<K: PartialEq<&'static str>, V>((k, _): &(K, V)) -> bool {
            *k != "cookie"
        }

        match self {
            Self::APIGatewayHttpEvent(t) => t.headers.iter().map(cloned).map(as_multi).collect(),
            Self::APIGatewayRestEvent(t) => t
                .multi_value_headers
                .iter()
                .filter(without_cookie)
                .map(cloned)
                .collect(),
            Self::APIGatewayWebSocketEvent(t) => t
                .multi_value_headers
                .iter()
                .filter(without_cookie)
                .map(cloned)
                .collect(),
            Self::ALBEvent(t) => {
                if t.multi_value_headers.is_empty() {
                    t.headers
                        .iter()
                        .filter(without_cookie)
                        .map(cloned)
                        .map(as_multi)
                        .collect()
                } else {
                    t.multi_value_headers
                        .iter()
                        .filter(without_cookie)
                        .map(cloned)
                        .collect()
                }
            }
            Self::LambdaFunctionUrlEvent(t) => t.headers.iter().map(cloned).map(as_multi).collect(),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => HashMap::default(),
        }
    }
    fn request_cookies(&self) -> HashMap<String, Vec<String>> {
        fn parse_cookie<'a>(
            cookie: impl Into<Cow<'a, str>>,
        ) -> impl Iterator<Item = (String, String)> {
            Cookie::split_parse(cookie)
                .filter_map(Result::ok)
                .map(|c| (c.name().to_string(), c.value().to_string()))
        }
        fn list_to_map(list: impl AsRef<[String]>) -> HashMap<String, Vec<String>> {
            list.as_ref()
                .iter()
                .filter_map(|c| c.split_once('='))
                .chunk_by(|(k, _)| (*k).to_string())
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(|(_, v)| v.to_string()).collect()))
                .collect()
        }

        match self {
            Self::APIGatewayHttpEvent(t) => t.cookies.as_ref().map(list_to_map).unwrap_or_default(),
            Self::APIGatewayRestEvent(t) => t
                .multi_value_headers
                .get("cookie")
                .cloned()
                .or_else(|| t.headers.get("cookie").map(|v| vec![v.clone()]))
                .unwrap_or_default()
                .iter()
                .flat_map(parse_cookie)
                .chunk_by(|(k, _)| (*k).clone())
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(|(_, v)| v.clone()).collect()))
                .collect(),
            Self::APIGatewayWebSocketEvent(t) => t
                .multi_value_headers
                .get("cookie")
                .cloned()
                .unwrap_or_default()
                .iter()
                .flat_map(parse_cookie)
                .chunk_by(|(k, _)| k.clone())
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(|(_, v)| v.clone()).collect()))
                .collect(),
            Self::ALBEvent(t) => t
                .multi_value_headers
                .get("cookie")
                .cloned()
                .or_else(|| t.headers.get("cookie").map(|v| vec![v.clone()]))
                .unwrap_or_default()
                .iter()
                .flat_map(parse_cookie)
                .chunk_by(|(k, _)| (*k).clone())
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(|(_, v)| v.clone()).collect()))
                .collect(),
            Self::LambdaFunctionUrlEvent(t) => {
                t.cookies.as_ref().map(list_to_map).unwrap_or_default()
            }
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => HashMap::default(),
        }
    }
    fn query_params(&self) -> HashMap<String, Vec<String>> {
        match self {
            Self::APIGatewayHttpEvent(t) => t
                .query_string_parameters
                .iter()
                .map(|(k, v)| (k.clone(), v.split(',').map(str::to_string).collect()))
                .collect(),
            Self::APIGatewayRestEvent(t) => t.query_parameters.clone(),
            Self::APIGatewayWebSocketEvent(t) => t.query_parameters.clone(),
            Self::ALBEvent(t) => {
                if t.multi_value_query_parameters.is_empty() {
                    t.query_parameters
                        .iter()
                        .map(|(k, v)| (k.clone(), vec![v.clone()]))
                        .collect()
                } else {
                    t.multi_value_query_parameters.clone()
                }
            }
            Self::LambdaFunctionUrlEvent(t) => t
                .query_string_parameters
                .iter()
                .map(|(k, v)| (k.clone(), vec![v.clone()]))
                .collect(),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => HashMap::default(),
        }
    }
    fn path_params(&self) -> HashMap<String, String> {
        match self {
            Self::APIGatewayHttpEvent(t) => t.path_parameters.clone(),
            Self::APIGatewayRestEvent(t) => t.path_parameters.clone(),
            Self::APIGatewayWebSocketEvent(t) => t.path_parameters.clone(),
            #[allow(clippy::match_same_arms)]
            Self::ALBEvent(_) => HashMap::default(),
            #[allow(clippy::match_same_arms)]
            Self::LambdaFunctionUrlEvent(_) => HashMap::default(),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => HashMap::default(),
        }
    }
    fn request_body<'a>(&'a self) -> Option<Box<dyn Read + 'a>> {
        match self {
            Self::APIGatewayHttpEvent(t) => t.body.reader().ok().flatten(),
            Self::APIGatewayRestEvent(t) => t.body.reader().ok().flatten(),
            Self::APIGatewayWebSocketEvent(t) => t.body.reader().ok().flatten(),
            Self::ALBEvent(t) => t.body.reader().ok().flatten(),
            Self::LambdaFunctionUrlEvent(t) => t.body.reader().ok().flatten(),
            // Events that are not relevant to App & API Protection
            Self::MSKEvent(_)
            | Self::SqsRecord(_)
            | Self::SnsRecord(_)
            | Self::DynamoDbRecord(_)
            | Self::S3Record(_)
            | Self::EventBridgeEvent(_)
            | Self::KinesisRecord(_)
            | Self::StepFunctionEvent(_)
            | Self::Unknown => None,
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

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
            Err(Error::WafRulesetParseError(_))
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
                Err(Error::WafInitializationFailed), // There is no rule nor processor in the ruleset
            ),
            concat!(
                "should have failed with ",
                stringify!(Error::WafCreationFailed),
                " but was {:?}"
            ),
            result
        );
    }
}
