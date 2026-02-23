// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::appsec::processor::Processor as AppSecProcessor;
use crate::appsec::processor::context::HoldArguments;
use crate::config;
use crate::lifecycle::invocation::processor::S_TO_MS;
use crate::tags::provider;
use crate::traces::span_pointers::{SpanPointer, attach_span_pointers_to_meta};
use crate::traces::{
    AWS_XRAY_DAEMON_ADDRESS_URL_PREFIX, DNS_LOCAL_HOST_ADDRESS_URL_PREFIX,
    DNS_NON_ROUTABLE_ADDRESS_URL_PREFIX, INVOCATION_SPAN_RESOURCE, LAMBDA_EXTENSION_URL_PREFIX,
    LAMBDA_RUNTIME_URL_PREFIX, LAMBDA_STATSD_URL_PREFIX,
};
use async_trait::async_trait;
use libdd_common::Endpoint;
use libdd_trace_obfuscation::obfuscate::obfuscate_span;
use libdd_trace_obfuscation::obfuscation_config;
use libdd_trace_protobuf::pb;
use libdd_trace_protobuf::pb::Span;
use libdd_trace_utils::send_data::{Compression, SendDataBuilder};
use libdd_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
use libdd_trace_utils::trace_utils::{self};
use libdd_trace_utils::tracer_header_tags;
use libdd_trace_utils::tracer_payload::{TraceChunkProcessor, TracerPayloadCollection};
use regex::Regex;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::error::SendError;
use tracing::{debug, error};

use super::stats_generator::StatsGenerator;
use super::trace_aggregator::SendDataBuilderInfo;

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceProcessor {
    pub obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
}

struct ChunkProcessor {
    config: Arc<config::Config>,
    obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    tags_provider: Arc<provider::Provider>,
    span_pointers: Option<Vec<SpanPointer>>,
}

impl TraceChunkProcessor for ChunkProcessor {
    fn process(&mut self, chunk: &mut pb::TraceChunk, root_span_index: usize) {
        if let Some(root_span) = chunk.spans.get(root_span_index)
            && filter_span_by_tags(root_span, &self.config)
        {
            chunk.spans.clear();
            return;
        }

        chunk
            .spans
            .retain(|span| !filter_span_from_lambda_library_or_runtime(span));
        for span in &mut chunk.spans {
            // Service name could be incorrectly set to 'aws.lambda'
            // in datadog lambda libraries
            if span.service == "aws.lambda"
                && let Some(service) = self.tags_provider.get_tags_map().get("service")
            {
                span.service.clone_from(service);
            }

            // Remove the _dd.base_service tag for unintentional service name override
            span.meta.remove("_dd.base_service");

            self.tags_provider.get_tags_map().iter().for_each(|(k, v)| {
                span.meta.insert(k.clone(), v.clone());
            });
            // TODO(astuyve) generalize this and delegate to an enum
            span.meta.insert("origin".to_string(), "lambda".to_string());
            span.meta
                .insert("_dd.origin".to_string(), "lambda".to_string());
            obfuscate_span(span, &self.obfuscation_config);
        }

        if let Some(span) = chunk.spans.get_mut(root_span_index) {
            attach_span_pointers_to_meta(&mut span.meta, &self.span_pointers);
        }
    }
}

fn filter_span_by_tags(span: &Span, config: &config::Config) -> bool {
    // Handle required tags from DD_APM_FILTER_TAGS_REQUIRE (exact match)
    if let Some(require_tags) = &config.apm_filter_tags_require
        && !require_tags.is_empty()
    {
        let matches_require = require_tags
            .iter()
            .all(|filter| span_matches_tag_exact_filter(span, filter));
        if !matches_require {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - doesn't match all required tags {}",
                span.name,
                require_tags.join(", ")
            );
            return true;
        }
    }

    // Handle required regex tags from DD_APM_FILTER_TAGS_REGEX_REQUIRE (regex match)
    if let Some(require_regex_tags) = &config.apm_filter_tags_regex_require
        && !require_regex_tags.is_empty()
    {
        let matches_require_regex = require_regex_tags
            .iter()
            .all(|filter| span_matches_tag_regex_filter(span, filter));
        if !matches_require_regex {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - doesn't match all required regex tags {}",
                span.name,
                require_regex_tags.join(", ")
            );
            return true;
        }
    }

    // Handle reject tags from DD_APM_FILTER_TAGS_REJECT (exact match)
    if let Some(reject_tags) = &config.apm_filter_tags_reject
        && !reject_tags.is_empty()
    {
        let matches_reject = reject_tags
            .iter()
            .any(|filter| span_matches_tag_exact_filter(span, filter));
        if matches_reject {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - matches reject tags {}",
                span.name,
                reject_tags.join(", ")
            );
            return true;
        }
    }

    // Handle reject regex tags from DD_APM_FILTER_TAGS_REGEX_REJECT (regex match)
    if let Some(reject_regex_tags) = &config.apm_filter_tags_regex_reject
        && !reject_regex_tags.is_empty()
    {
        let matches_reject_regex = reject_regex_tags
            .iter()
            .any(|filter| span_matches_tag_regex_filter(span, filter));
        if matches_reject_regex {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - matches reject regex tags {}",
                span.name,
                reject_regex_tags.join(", ")
            );
            return true;
        }
    }

    false
}

fn span_matches_tag_exact_filter(span: &Span, filter: &str) -> bool {
    let parts: Vec<&str> = filter.splitn(2, ':').collect();

    if parts.len() == 2 {
        let key = parts[0].trim();
        let value = parts[1].trim();
        span_matches_tag_exact(span, key, value)
    } else if parts.len() == 1 {
        let key = parts[0].trim();
        span_has_key(span, key)
    } else {
        false
    }
}

fn span_matches_tag_regex_filter(span: &Span, filter: &str) -> bool {
    let parts: Vec<&str> = filter.splitn(2, ':').collect();

    if parts.len() == 2 {
        let key = parts[0].trim();
        let value = parts[1].trim();
        span_matches_tag_regex(span, key, value)
    } else if parts.len() == 1 {
        let key = parts[0].trim();
        span_has_key(span, key)
    } else {
        false
    }
}

fn span_matches_tag_exact(span: &Span, key: &str, value: &str) -> bool {
    if let Some(span_value) = span.meta.get(key)
        && span_value == value
    {
        return true;
    }

    let span_property_value = match key {
        "name" => Some(&span.name),
        "service" => Some(&span.service),
        "resource" => Some(&span.resource),
        "type" => Some(&span.r#type),
        _ => None,
    };

    if let Some(prop_value) = span_property_value
        && prop_value == value
    {
        return true;
    }

    false
}

fn span_matches_tag_regex(span: &Span, key: &str, value: &str) -> bool {
    let Ok(regex) = Regex::new(value) else {
        debug!(
            "TRACE_PROCESSOR | Invalid regex pattern '{}' for key '{}', treating as non-match",
            value, key
        );
        return false;
    };

    if let Some(span_value) = span.meta.get(key)
        && regex.is_match(span_value)
    {
        return true;
    }

    let span_property_value = match key {
        "name" => Some(&span.name),
        "service" => Some(&span.service),
        "resource" => Some(&span.resource),
        "type" => Some(&span.r#type),
        _ => None,
    };
    if let Some(prop_value) = span_property_value
        && regex.is_match(prop_value)
    {
        return true;
    }

    false
}

fn span_has_key(span: &Span, key: &str) -> bool {
    if span.meta.contains_key(key) {
        return true;
    }
    match key {
        "name" => !span.name.is_empty(),
        "service" => !span.service.is_empty(),
        "resource" => !span.resource.is_empty(),
        "type" => !span.r#type.is_empty(),
        _ => false,
    }
}

fn filter_span_from_lambda_library_or_runtime(span: &Span) -> bool {
    if let Some(url) = span.meta.get("http.url")
        && (url.starts_with(LAMBDA_RUNTIME_URL_PREFIX)
            || url.starts_with(LAMBDA_EXTENSION_URL_PREFIX)
            || url.starts_with(LAMBDA_STATSD_URL_PREFIX))
    {
        return true;
    }

    if let (Some(tcp_host), Some(tcp_port)) = (
        span.meta.get("tcp.remote.host"),
        span.meta.get("tcp.remote.port"),
    ) {
        {
            let tcp_lambda_url_prefix = format!("http://{tcp_host}:{tcp_port}");
            if tcp_lambda_url_prefix.starts_with(LAMBDA_RUNTIME_URL_PREFIX)
                || tcp_lambda_url_prefix.starts_with(LAMBDA_EXTENSION_URL_PREFIX)
                || tcp_lambda_url_prefix.starts_with(LAMBDA_STATSD_URL_PREFIX)
            {
                return true;
            }
        }
    }

    if let Some(dns_address) = span.meta.get("dns.address")
        && (dns_address.starts_with(DNS_NON_ROUTABLE_ADDRESS_URL_PREFIX)
            || dns_address.starts_with(DNS_LOCAL_HOST_ADDRESS_URL_PREFIX)
            || dns_address.starts_with(AWS_XRAY_DAEMON_ADDRESS_URL_PREFIX))
    {
        return true;
    }
    if span.resource == INVOCATION_SPAN_RESOURCE {
        return true;
    }

    if span.name == "dns.lookup"
        || span.resource == DNS_LOCAL_HOST_ADDRESS_URL_PREFIX
        || span.resource == DNS_NON_ROUTABLE_ADDRESS_URL_PREFIX
    {
        return true;
    }

    false
}

#[allow(clippy::module_name_repetitions)]
#[allow(clippy::too_many_arguments)]
#[async_trait]
pub trait TraceProcessor {
    fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags<'_>,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        span_pointers: Option<Vec<SpanPointer>>,
    ) -> (SendDataBuilderInfo, TracerPayloadCollection);
}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags<'_>,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        span_pointers: Option<Vec<SpanPointer>>,
    ) -> (SendDataBuilderInfo, TracerPayloadCollection) {
        let mut payload = trace_utils::collect_pb_trace_chunks(
            traces,
            &header_tags,
            &mut ChunkProcessor {
                config: config.clone(),
                obfuscation_config: self.obfuscation_config.clone(),
                tags_provider: tags_provider.clone(),
                span_pointers,
            },
            true, // send agentless since we are the agent
        )
        .unwrap_or_else(|e| {
            error!("TRACE_PROCESSOR | Error processing traces: {:?}", e);
            TracerPayloadCollection::V07(vec![])
        });
        if let TracerPayloadCollection::V07(ref mut collection) = payload {
            // add function tags to all payloads in this TracerPayloadCollection
            let tags = tags_provider.get_function_tags_map();
            for tracer_payload in collection.iter_mut() {
                tracer_payload.tags.extend(tags.clone());
            }
        }
        let endpoint = Endpoint {
            url: hyper::Uri::from_str(&config.apm_dd_url)
                .expect("can't parse trace intake URL, exiting"),
            // Will be set at flush time
            api_key: None,
            timeout_ms: config.flush_timeout * S_TO_MS,
            test_token: None,
            use_system_resolver: false,
        };

        // Clone inner V07 payloads for stats generation (TracerPayload is Clone,
        // but TracerPayloadCollection is not).
        let payloads_for_stats = match &payload {
            TracerPayloadCollection::V07(payloads) => {
                TracerPayloadCollection::V07(payloads.clone())
            }
            other => {
                error!("TRACE_PROCESSOR | Unexpected payload type for stats: {other:?}");
                TracerPayloadCollection::V07(vec![])
            }
        };

        // Move original payload into builder (no clone needed)
        let builder = SendDataBuilder::new(body_size, payload, header_tags, &endpoint)
            .with_compression(Compression::Zstd(config.apm_config_compression_level))
            .with_retry_strategy(RetryStrategy::new(
                1,
                100,
                RetryBackoffType::Exponential,
                None,
            ));

        (
            SendDataBuilderInfo::new(builder, body_size),
            payloads_for_stats,
        )
    }
}

/// A utility that is used to process, then send traces to the trace aggregator.
///
/// This applies [`AppSecProcessor::process_span`] on the `aws.lambda` span
/// contained in the traces (if any), and may buffer the traces if the
/// [`AppSecProcessor`] has not yet seen the corresponding response payload.
///
/// Once ready to flush, the traces are submitted to the provided [`Sender`].
#[derive(Clone)]
pub struct SendingTraceProcessor {
    /// The [`AppSecProcessor`] to use for security-processing the traces, if
    /// configured.
    pub appsec: Option<Arc<Mutex<AppSecProcessor>>>,
    /// The [`TraceProcessor`]  to use for transforming raw traces into
    /// [`SendDataBuilderInfo`]s before flushing.
    pub processor: Arc<dyn TraceProcessor + Send + Sync>,
    /// The [`Sender`] to use for flushing the traces to the trace aggregator.
    pub trace_tx: Sender<SendDataBuilderInfo>,
    /// The [`StatsGenerator`] to use for generating stats and sending them to
    /// the stats concentrator.
    pub stats_generator: Arc<StatsGenerator>,
}
impl SendingTraceProcessor {
    /// Processes the provided traces, then flushes them to the trace aggregator
    /// for sending to the backend.
    pub async fn send_processed_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags<'_>,
        mut traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        span_pointers: Option<Vec<SpanPointer>>,
    ) -> Result<(), SendError<SendDataBuilderInfo>> {
        traces = if let Some(appsec) = &self.appsec {
            let mut appsec = appsec.lock().await;
            traces.into_iter().filter_map(|mut trace| {
                let Some(span) = AppSecProcessor::service_entry_span_mut(&mut trace) else {
                    return Some(trace);
                };

                let (finalized, ctx) = appsec.process_span(span);
                if  finalized {
                    Some(trace)
                } else if let Some(ctx) = ctx{
                    debug!("TRACE_PROCESSOR | Holding trace for App & API Protection additional data");
                    ctx.hold_trace(trace, SendingTraceProcessor{ appsec:  None, processor: self.processor.clone(), trace_tx: self.trace_tx.clone(), stats_generator: self.stats_generator.clone() }, HoldArguments{
                        config:Arc::clone(&config),
                        tags_provider:Arc::clone(&tags_provider),
                        body_size,
                        span_pointers:span_pointers.clone(),
                        tracer_header_tags_lang: header_tags.lang.to_string(),
                        tracer_header_tags_lang_version: header_tags.lang_version.to_string(),
                        tracer_header_tags_lang_interpreter: header_tags.lang_interpreter.to_string(),
                        tracer_header_tags_lang_vendor: header_tags.lang_vendor.to_string(),
                        tracer_header_tags_tracer_version: header_tags.tracer_version.to_string(),
                        tracer_header_tags_container_id: header_tags.container_id.to_string(),
                        tracer_header_tags_client_computed_top_level: header_tags.client_computed_top_level,
                        tracer_header_tags_client_computed_stats: header_tags.client_computed_stats,
                        tracer_header_tags_dropped_p0_traces: header_tags.dropped_p0_traces,
                        tracer_header_tags_dropped_p0_spans: header_tags.dropped_p0_spans,
                    });
                    None
                } else {
                    Some(trace)
                }
            }).collect()
        } else {
            traces
        };

        if traces.is_empty() {
            debug!("TRACE_PROCESSOR | no traces left to be sent, skipping...");
            return Ok(());
        }

        let (payload, processed_traces) = self.processor.process_traces(
            config.clone(),
            tags_provider,
            header_tags,
            traces,
            body_size,
            span_pointers,
        );
        self.trace_tx.send(payload).await?;

        // This needs to be after process_traces() because process_traces()
        // performs obfuscation, and we need to compute stats on the obfuscated traces.
        if config.compute_trace_stats_on_extension
            && let Err(err) = self.stats_generator.send(&processed_traces)
        {
            // Just log the error. We don't think trace stats are critical, so we don't want to
            // return an error if only stats fail to send.
            error!("TRACE_PROCESSOR | Error sending traces to the stats concentrator: {err}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        time::{SystemTime, UNIX_EPOCH},
    };

    use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;

    use crate::{LAMBDA_RUNTIME_SLUG, config::Config, tags::provider::Provider};

    use super::*;

    fn get_current_timestamp_nanos() -> i64 {
        i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_nanos(),
        )
        .expect("can't parse time")
    }

    fn create_test_config() -> Arc<Config> {
        Arc::new(Config {
            apm_dd_url: "https://trace.agent.datadoghq.com".to_string(),
            service: Some("test-service".to_string()),
            tags: HashMap::from([
                ("test".to_string(), "tag".to_string()),
                ("env".to_string(), "test-env".to_string()),
            ]),
            ..Config::default()
        })
    }

    fn create_tags_provider(config: Arc<Config>) -> Arc<Provider> {
        let mut metadata = HashMap::new();
        metadata.insert(
            "function_arn".to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        );
        let provider = Provider::new(config, LAMBDA_RUNTIME_SLUG.to_string(), &metadata);
        Arc::new(provider)
    }
    fn create_test_span(
        trace_id: u64,
        span_id: u64,
        parent_id: u64,
        start: i64,
        is_top_level: bool,
        tags_provider: Arc<Provider>,
    ) -> pb::Span {
        let mut meta: HashMap<String, String> = tags_provider.get_tags_map().clone();
        meta.insert(
            "runtime-id".to_string(),
            "test-runtime-id-value".to_string(),
        );

        let mut span = pb::Span {
            trace_id,
            span_id,
            service: "test-service".to_string(),
            name: "test_name".to_string(),
            resource: "test-resource".to_string(),
            parent_id,
            start,
            duration: 5,
            error: 0,
            meta: meta.clone(),
            metrics: HashMap::new(),
            r#type: String::new(),
            meta_struct: HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        };
        if is_top_level {
            span.metrics.insert("_top_level".to_string(), 1.0);
            span.meta
                .insert("_dd.origin".to_string(), "lambda".to_string());
            span.meta.insert("origin".to_string(), "lambda".to_string());
            span.meta
                .insert("functionname".to_string(), "my-function".to_string());
            span.r#type = String::new();
        }
        span
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    #[cfg_attr(miri, ignore)]
    async fn test_process_trace() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let start = get_current_timestamp_nanos();

        let tags_provider = create_tags_provider(create_test_config());
        let span = create_test_span(11, 222, 333, start, true, tags_provider);

        let traces: Vec<Vec<pb::Span>> = vec![vec![span.clone()]];

        let header_tags = tracer_header_tags::TracerHeaderTags {
            lang: "nodejs",
            lang_version: "v19.7.0",
            lang_interpreter: "v8",
            lang_vendor: "vendor",
            tracer_version: "4.0.0",
            container_id: "33",
            client_computed_top_level: false,
            client_computed_stats: false,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };

        let trace_processor = ServerlessTraceProcessor {
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
        };
        let config = create_test_config();
        let tags_provider = create_tags_provider(config.clone());
        let (tracer_payload, _processed_traces) = trace_processor.process_traces(
            config,
            tags_provider.clone(),
            header_tags,
            traces,
            100,
            None,
        );

        let expected_tracer_payload = pb::TracerPayload {
            container_id: "33".to_string(),
            language_name: "nodejs".to_string(),
            language_version: "v19.7.0".to_string(),
            tracer_version: "4.0.0".to_string(),
            runtime_id: "test-runtime-id-value".to_string(),
            chunks: vec![pb::TraceChunk {
                priority: i32::from(i8::MIN),
                origin: "lambda".to_string(),
                spans: vec![span.clone()],
                tags: HashMap::new(),
                dropped_trace: false,
            }],
            tags: tags_provider.get_function_tags_map(),
            env: "test-env".to_string(),
            hostname: String::new(),
            app_version: String::new(),
        };

        let received_payload = if let TracerPayloadCollection::V07(payload) =
            tracer_payload.builder.build().get_payloads()
        {
            Some(payload[0].clone())
        } else {
            None
        };

        assert_eq!(
            expected_tracer_payload,
            received_payload.expect("no payload received")
        );
    }

    #[test]
    fn test_span_matches_tag_exact() {
        let span = Span {
            service: "my-service".to_string(),
            meta: {
                let mut meta = std::collections::HashMap::new();
                meta.insert("env".to_string(), "production".to_string());
                meta
            },
            ..Default::default()
        };

        assert!(span_matches_tag_exact(&span, "env", "production"));
        assert!(!span_matches_tag_exact(&span, "env", "")); // Empty string doesn't match non-empty value
        assert!(!span_matches_tag_exact(&span, "env", "development"));

        assert!(span_matches_tag_exact(&span, "service", "my-service"));
        assert!(!span_matches_tag_exact(&span, "service", "")); // Empty string doesn't match non-empty value
        assert!(!span_matches_tag_exact(&span, "service", "other-service"));
    }

    #[test]
    fn test_span_matches_tag_regex() {
        let span = Span {
            service: "user-service".to_string(),
            meta: {
                let mut meta = std::collections::HashMap::new();
                meta.insert("env".to_string(), "production".to_string());
                meta
            },
            ..Default::default()
        };

        assert!(span_matches_tag_regex(&span, "env", r"prod.*"));
        assert!(span_matches_tag_regex(&span, "env", ""));
        assert!(!span_matches_tag_regex(&span, "env", r"dev.*"));

        assert!(span_matches_tag_regex(&span, "service", r".*-service"));
        assert!(span_matches_tag_regex(&span, "service", ""));
        assert!(!span_matches_tag_regex(&span, "service", r"api-.*"));

        assert!(!span_matches_tag_regex(&span, "env", r"[unclosed"));
    }

    #[test]
    fn test_span_matches_tag_with_config() {
        let span = Span {
            service: "user-service".to_string(),
            meta: {
                let mut meta = std::collections::HashMap::new();
                meta.insert("env".to_string(), "test-environment".to_string());
                meta
            },
            ..Default::default()
        };

        assert!(span_matches_tag_exact(&span, "env", "test-environment"));
        assert!(!span_matches_tag_exact(&span, "env", "test"));

        assert!(span_matches_tag_regex(&span, "env", r"test.*"));
        assert!(!span_matches_tag_regex(&span, "env", r"prod.*"));
    }

    #[test]
    fn test_filter_span_require_tags() {
        let config = Config {
            apm_filter_tags_require: Some(vec![
                "env:production".to_string(),
                "service:api".to_string(),
            ]),
            ..Default::default()
        };

        // Span that matches ALL of the require tags
        let mut span_match_all = Span::default();
        span_match_all
            .meta
            .insert("env".to_string(), "production".to_string());
        span_match_all
            .meta
            .insert("service".to_string(), "api".to_string());
        assert!(!filter_span_by_tags(&span_match_all, &config));

        // Span that matches only one of the require tags
        let mut span_partial_match = Span::default();
        span_partial_match
            .meta
            .insert("env".to_string(), "production".to_string());
        assert!(filter_span_by_tags(&span_partial_match, &config));

        // Span that doesn't match any require tags
        let mut span_no_match = Span::default();
        span_no_match
            .meta
            .insert("env".to_string(), "development".to_string());
        assert!(filter_span_by_tags(&span_no_match, &config));
    }

    #[test]
    fn test_filter_span_reject_tags() {
        let config = Config {
            apm_filter_tags_reject: Some(vec!["debug:true".to_string(), "env:test".to_string()]),
            ..Default::default()
        };

        // Span that matches a reject tag
        let mut span_reject = Span::default();
        span_reject
            .meta
            .insert("debug".to_string(), "true".to_string());
        span_reject
            .meta
            .insert("env".to_string(), "production".to_string());
        assert!(filter_span_by_tags(&span_reject, &config));

        // Span that doesn't match any reject tags
        let mut span_keep = Span::default();
        span_keep
            .meta
            .insert("env".to_string(), "production".to_string());
        assert!(!filter_span_by_tags(&span_keep, &config));
    }

    #[test]
    fn test_filter_span_require_and_reject_tags() {
        let config = Config {
            apm_filter_tags_require: Some(vec!["env:production".to_string()]),
            apm_filter_tags_reject: Some(vec!["debug:true".to_string()]),
            ..Default::default()
        };

        // Span that matches require but also matches reject
        let mut span_both = Span::default();
        span_both
            .meta
            .insert("env".to_string(), "production".to_string());
        span_both
            .meta
            .insert("debug".to_string(), "true".to_string());
        assert!(filter_span_by_tags(&span_both, &config));

        // Span that matches require and doesn't match reject
        let mut span_good = Span::default();
        span_good
            .meta
            .insert("env".to_string(), "production".to_string());
        assert!(!filter_span_by_tags(&span_good, &config));
    }

    #[test]
    fn test_root_span_filtering_drops_entire_trace() {
        use crate::tags::provider::Provider;
        use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
        use std::sync::Arc;

        let root_span = pb::Span {
            name: "lambda.invoke".to_string(),
            service: "my-service".to_string(),
            resource: "my-resource".to_string(),
            trace_id: 123,
            span_id: 456,
            parent_id: 0,
            start: 1000,
            duration: 5000,
            error: 0,
            meta: {
                let mut meta = std::collections::HashMap::new();
                meta.insert("env".to_string(), "test".to_string());
                meta
            },
            metrics: std::collections::HashMap::new(),
            r#type: String::new(),
            span_links: vec![],
            meta_struct: std::collections::HashMap::new(),
            span_events: vec![],
        };

        let child_span = pb::Span {
            name: "child.operation".to_string(),
            service: "my-service".to_string(),
            resource: "child-resource".to_string(),
            trace_id: 123,
            span_id: 789,
            parent_id: 456,
            start: 1100,
            duration: 1000,
            error: 0,
            meta: std::collections::HashMap::new(),
            metrics: std::collections::HashMap::new(),
            r#type: String::new(),
            span_links: vec![],
            meta_struct: std::collections::HashMap::new(),
            span_events: vec![],
        };

        let mut chunk = pb::TraceChunk {
            priority: 1,
            origin: "lambda".to_string(),
            spans: vec![root_span, child_span],
            tags: std::collections::HashMap::new(),
            dropped_trace: false,
        };

        let config = Arc::new(Config {
            apm_filter_tags_reject: Some(vec!["env:test".to_string()]),
            ..Default::default()
        });

        let mut processor = ChunkProcessor {
            config: config.clone(),
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
            tags_provider: Arc::new(Provider::new(
                config,
                "lambda".to_string(),
                &std::collections::HashMap::from([(
                    "function_arn".to_string(),
                    "test-arn".to_string(),
                )]),
            )),
            span_pointers: None,
        };

        processor.process(&mut chunk, 0);

        assert_eq!(
            chunk.spans.len(),
            0,
            "Entire trace should be dropped when root span matches filter rules"
        );
    }

    #[test]
    fn test_root_span_filtering_allows_trace_when_no_match() {
        use crate::tags::provider::Provider;
        use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
        use std::sync::Arc;

        let root_span = pb::Span {
            name: "lambda.invoke".to_string(),
            service: "my-service".to_string(),
            resource: "my-resource".to_string(),
            trace_id: 123,
            span_id: 456,
            parent_id: 0,
            start: 1000,
            duration: 5000,
            error: 0,
            meta: {
                let mut meta = std::collections::HashMap::new();
                meta.insert("env".to_string(), "production".to_string());
                meta
            },
            metrics: std::collections::HashMap::new(),
            r#type: String::new(),
            span_links: vec![],
            meta_struct: std::collections::HashMap::new(),
            span_events: vec![],
        };

        let child_span = pb::Span {
            name: "child.operation".to_string(),
            service: "my-service".to_string(),
            resource: "child-resource".to_string(),
            trace_id: 123,
            span_id: 789,
            parent_id: 456,
            start: 1100,
            duration: 1000,
            error: 0,
            meta: std::collections::HashMap::new(),
            metrics: std::collections::HashMap::new(),
            r#type: String::new(),
            span_links: vec![],
            meta_struct: std::collections::HashMap::new(),
            span_events: vec![],
        };

        let mut chunk = pb::TraceChunk {
            priority: 1,
            origin: "lambda".to_string(),
            spans: vec![root_span, child_span],
            tags: std::collections::HashMap::new(),
            dropped_trace: false,
        };

        let config = Arc::new(Config {
            apm_filter_tags_reject: Some(vec!["env:test".to_string()]),
            ..Default::default()
        });

        let mut processor = ChunkProcessor {
            config: config.clone(),
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
            tags_provider: Arc::new(Provider::new(
                config,
                "lambda".to_string(),
                &std::collections::HashMap::from([(
                    "function_arn".to_string(),
                    "test-arn".to_string(),
                )]),
            )),
            span_pointers: None,
        };

        processor.process(&mut chunk, 0);

        assert_eq!(
            chunk.spans.len(),
            2,
            "Trace should be kept when root span doesn't match filter rules"
        );
    }

    #[test]
    fn test_root_span_filtering_allows_trace_when_no_filter_tags() {
        use crate::tags::provider::Provider;
        use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
        use std::sync::Arc;

        let root_span = pb::Span {
            name: "lambda.invoke".to_string(),
            service: "my-service".to_string(),
            resource: "my-resource".to_string(),
            trace_id: 123,
            span_id: 456,
            parent_id: 0,
            start: 1000,
            duration: 5000,
            error: 0,
            meta: {
                let mut meta = std::collections::HashMap::new();
                meta.insert("env".to_string(), "production".to_string());
                meta
            },
            metrics: std::collections::HashMap::new(),
            r#type: String::new(),
            span_links: vec![],
            meta_struct: std::collections::HashMap::new(),
            span_events: vec![],
        };

        let child_span = pb::Span {
            name: "child.operation".to_string(),
            service: "my-service".to_string(),
            resource: "child-resource".to_string(),
            trace_id: 123,
            span_id: 789,
            parent_id: 456,
            start: 1100,
            duration: 1000,
            error: 0,
            meta: std::collections::HashMap::new(),
            metrics: std::collections::HashMap::new(),
            r#type: String::new(),
            span_links: vec![],
            meta_struct: std::collections::HashMap::new(),
            span_events: vec![],
        };

        let mut chunk = pb::TraceChunk {
            priority: 1,
            origin: "lambda".to_string(),
            spans: vec![root_span, child_span],
            tags: std::collections::HashMap::new(),
            dropped_trace: false,
        };

        let config = Arc::new(Config {
            ..Default::default()
        });

        let mut processor = ChunkProcessor {
            config: config.clone(),
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
            tags_provider: Arc::new(Provider::new(
                config,
                "lambda".to_string(),
                &std::collections::HashMap::from([(
                    "function_arn".to_string(),
                    "test-arn".to_string(),
                )]),
            )),
            span_pointers: None,
        };

        processor.process(&mut chunk, 0);

        assert_eq!(
            chunk.spans.len(),
            2,
            "Trace should be kept when no filter tags are set."
        );
    }
}
