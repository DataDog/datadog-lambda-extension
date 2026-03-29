// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::appsec::processor::Processor as AppSecProcessor;
use crate::appsec::processor::context::HoldArguments;
use crate::config;
use crate::lifecycle::invocation::processor::S_TO_MS;
use crate::tags::lambda::tags::COMPUTE_STATS_KEY;
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
use tracing::{debug, error, warn};

use crate::traces::stats_generator::StatsGenerator;
use crate::traces::trace_aggregator::{OwnedTracerHeaderTags, SendDataBuilderInfo};
use libdd_trace_normalization::normalizer::SamplerPriority;

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceProcessor {
    pub obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    tag_filters: Arc<TagFilters>,
}

#[derive(Clone, Debug)]
struct ExactFilter {
    key: String,
    value: Option<String>,
}

#[derive(Clone, Debug)]
struct RegexFilter {
    key: String,
    regex: Option<Regex>,
}

#[derive(Clone, Debug, Default)]
struct TagFilters {
    require: Vec<ExactFilter>,
    reject: Vec<ExactFilter>,
    require_regex: Vec<RegexFilter>,
    reject_regex: Vec<RegexFilter>,
}

impl TagFilters {
    fn from_config(config: &config::Config) -> Self {
        Self {
            require: compile_exact_filters(config.apm_filter_tags_require.as_deref()),
            reject: compile_exact_filters(config.apm_filter_tags_reject.as_deref()),
            require_regex: compile_regex_filters(config.apm_filter_tags_regex_require.as_deref()),
            reject_regex: compile_regex_filters(config.apm_filter_tags_regex_reject.as_deref()),
        }
    }
}

struct ChunkProcessor {
    obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    tags_provider: Arc<provider::Provider>,
    span_pointers: Option<Vec<SpanPointer>>,
    tag_filters: Arc<TagFilters>,
}

impl ServerlessTraceProcessor {
    #[must_use]
    pub fn new(
        config: &config::Config,
        obfuscation_config: obfuscation_config::ObfuscationConfig,
    ) -> Self {
        Self {
            obfuscation_config: Arc::new(obfuscation_config),
            tag_filters: Arc::new(TagFilters::from_config(config)),
        }
    }
}

fn compile_exact_filters(filters: Option<&[String]>) -> Vec<ExactFilter> {
    filters
        .into_iter()
        .flatten()
        .map(|filter| match filter.split_once(':') {
            Some((key, value)) => ExactFilter {
                key: key.trim().to_string(),
                value: Some(value.trim().to_string()),
            },
            None => ExactFilter {
                key: filter.trim().to_string(),
                value: None,
            },
        })
        .collect()
}

fn compile_regex_filters(filters: Option<&[String]>) -> Vec<RegexFilter> {
    filters
        .into_iter()
        .flatten()
        .filter_map(|filter| match filter.split_once(':') {
            Some((key, pattern)) => if let Ok(regex) = Regex::new(pattern.trim()) {
                Some(RegexFilter {
                    key: key.trim().to_string(),
                    regex: Some(regex),
                })
            } else {
                warn!(
                    "TRACE_PROCESSOR | Invalid regex pattern '{}' for key '{}', skipping filter",
                    pattern.trim(),
                    key.trim()
                );
                None
            },
            None => Some(RegexFilter {
                key: filter.trim().to_string(),
                regex: None,
            }),
        })
        .collect()
}

fn format_exact_filter(filter: &ExactFilter) -> String {
    match &filter.value {
        None => filter.key.clone(),
        Some(value) => format!("{}:{}", filter.key, value),
    }
}

fn format_regex_filter(filter: &RegexFilter) -> String {
    match &filter.regex {
        None => filter.key.clone(),
        Some(regex) => format!("{}:{}", filter.key, regex.as_str()),
    }
}

fn span_matches_exact_filter(span: &Span, filter: &ExactFilter) -> bool {
    match &filter.value {
        None => span_has_key(span, &filter.key),
        Some(value) => span_matches_tag_exact(span, &filter.key, value),
    }
}

fn span_matches_regex_filter(span: &Span, filter: &RegexFilter) -> bool {
    match &filter.regex {
        None => span_has_key(span, &filter.key),
        Some(regex) => span_matches_tag_regex(span, &filter.key, regex),
    }
}

impl TraceChunkProcessor for ChunkProcessor {
    fn process(&mut self, chunk: &mut pb::TraceChunk, root_span_index: usize) {
        if let Some(root_span) = chunk.spans.get(root_span_index)
            && filter_span_by_tags(root_span, &self.tag_filters)
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

fn filter_span_by_tags(span: &Span, tag_filters: &TagFilters) -> bool {
    // Handle required tags from DD_APM_FILTER_TAGS_REQUIRE (exact match)
    if !tag_filters.require.is_empty() {
        let matches_require = tag_filters
            .require
            .iter()
            .all(|filter| span_matches_exact_filter(span, filter));
        if !matches_require {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - doesn't match all required tags {}",
                span.name,
                tag_filters
                    .require
                    .iter()
                    .map(format_exact_filter)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return true;
        }
    }

    // Handle required regex tags from DD_APM_FILTER_TAGS_REGEX_REQUIRE (regex match)
    if !tag_filters.require_regex.is_empty() {
        let matches_require_regex = tag_filters
            .require_regex
            .iter()
            .all(|filter| span_matches_regex_filter(span, filter));
        if !matches_require_regex {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - doesn't match all required regex tags {}",
                span.name,
                tag_filters
                    .require_regex
                    .iter()
                    .map(format_regex_filter)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return true;
        }
    }

    // Handle reject tags from DD_APM_FILTER_TAGS_REJECT (exact match)
    if !tag_filters.reject.is_empty() {
        let matches_reject = tag_filters
            .reject
            .iter()
            .any(|filter| span_matches_exact_filter(span, filter));
        if matches_reject {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - matches reject tags {}",
                span.name,
                tag_filters
                    .reject
                    .iter()
                    .map(format_exact_filter)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return true;
        }
    }

    // Handle reject regex tags from DD_APM_FILTER_TAGS_REGEX_REJECT (regex match)
    if !tag_filters.reject_regex.is_empty() {
        let matches_reject_regex = tag_filters
            .reject_regex
            .iter()
            .any(|filter| span_matches_regex_filter(span, filter));
        if matches_reject_regex {
            debug!(
                "TRACE_PROCESSOR | Filtering out span '{}' - matches reject regex tags {}",
                span.name,
                tag_filters
                    .reject_regex
                    .iter()
                    .map(format_regex_filter)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return true;
        }
    }

    false
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

fn span_matches_tag_regex(span: &Span, key: &str, regex: &Regex) -> bool {
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
    ) -> (Option<SendDataBuilderInfo>, TracerPayloadCollection);
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
    ) -> (Option<SendDataBuilderInfo>, TracerPayloadCollection) {
        let mut payload = trace_utils::collect_pb_trace_chunks(
            traces,
            &header_tags,
            &mut ChunkProcessor {
                obfuscation_config: self.obfuscation_config.clone(),
                tags_provider: tags_provider.clone(),
                span_pointers,
                tag_filters: self.tag_filters.clone(),
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
                // Tell the backend whether to compute stats:
                // - "1" (compute on backend) if neither the tracer nor the extension is computing them
                // - "0" (skip on backend) if the extension or the tracer has already computed them
                let compute_stats = if !config.compute_trace_stats_on_extension
                    && !header_tags.client_computed_stats
                {
                    "1"
                } else {
                    "0"
                };
                tracer_payload
                    .tags
                    .insert(COMPUTE_STATS_KEY.to_string(), compute_stats.to_string());
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
        // This clone is done BEFORE filtering so stats always include sampled-out traces.
        let payloads_for_stats = match &payload {
            TracerPayloadCollection::V07(payloads) => {
                TracerPayloadCollection::V07(payloads.clone())
            }
            other => {
                error!("TRACE_PROCESSOR | Unexpected payload type for stats: {other:?}");
                TracerPayloadCollection::V07(vec![])
            }
        };

        // Remove sampled-out chunks so they won't be sent to Datadog.
        // Sampled-out chunks are preserved in payloads_for_stats above so their
        // stats are still counted. SamplerPriority::None (-128) means no explicit priority
        // was set and the trace is kept; drop priorities are SamplerPriority::AutoDrop (0)
        // and UserDrop (-1, not represented in SamplerPriority).
        let body_size = if config.compute_trace_stats_on_extension
            && let TracerPayloadCollection::V07(ref mut tracer_payloads) = payload
        {
            for tp in tracer_payloads.iter_mut() {
                tp.chunks.retain(|chunk| {
                    chunk.priority > 0 || chunk.priority == SamplerPriority::None as i32
                });
            }
            tracer_payloads.retain(|tp| !tp.chunks.is_empty());
            if tracer_payloads.is_empty() {
                return (None, payloads_for_stats);
            }

            // Update body_size after dropping sampled-out traces.
            // Use the protobuf-encoded size of the filtered payload so the
            // TraceAggregator's 3.2 MB batch limit reflects only the data that
            // will actually be sent to the backend.
            tracer_payloads
                .iter()
                .map(prost::Message::encoded_len)
                .sum()
        } else {
            body_size
        };

        let owned_header_tags = OwnedTracerHeaderTags::from(header_tags.clone());

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
            Some(SendDataBuilderInfo::new(
                builder,
                body_size,
                owned_header_tags,
            )),
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

        let client_computed_stats = header_tags.client_computed_stats;
        let (payload, processed_traces) = self.processor.process_traces(
            config.clone(),
            tags_provider,
            header_tags,
            traces,
            body_size,
            span_pointers,
        );

        // This needs to be after process_traces() because process_traces()
        // performs obfuscation, and we need to compute stats on the obfuscated traces.
        // Skip if the tracer has already computed stats (Datadog-Client-Computed-Stats header).
        if config.compute_trace_stats_on_extension
            && !client_computed_stats
            && let Err(err) = self.stats_generator.send(&processed_traces)
        {
            // Just log the error. We don't think trace stats are critical, so we don't want to
            // return an error if only stats fail to send.
            error!("TRACE_PROCESSOR | Error sending traces to the stats concentrator: {err}");
        }

        if let Some(payload) = payload {
            self.trace_tx.send(payload).await?;
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

        let config = create_test_config();
        let trace_processor = ServerlessTraceProcessor::new(
            config.as_ref(),
            ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
        );
        let tags_provider = create_tags_provider(config.clone());
        let (tracer_payload, _processed_traces) = trace_processor.process_traces(
            config,
            tags_provider.clone(),
            header_tags,
            traces,
            100,
            None,
        );
        let tracer_payload = tracer_payload.expect("expected Some payload");

        let mut expected_tags = tags_provider.get_function_tags_map();
        // process_traces always sets _dd.compute_stats:"1"
        // because compute_trace_stats_on_extension is false and client_computed_stats is false.
        expected_tags.insert(COMPUTE_STATS_KEY.to_string(), "1".to_string());
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
            tags: expected_tags,
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

        assert!(span_matches_tag_regex(
            &span,
            "env",
            &Regex::new(r"prod.*").expect("regex pattern should be valid")
        ));
        assert!(span_matches_tag_regex(
            &span,
            "env",
            &Regex::new("").expect("regex pattern should be valid")
        ));
        assert!(!span_matches_tag_regex(
            &span,
            "env",
            &Regex::new(r"dev.*").expect("regex pattern should be valid")
        ));

        assert!(span_matches_tag_regex(
            &span,
            "service",
            &Regex::new(r".*-service").expect("regex pattern should be valid")
        ));
        assert!(span_matches_tag_regex(
            &span,
            "service",
            &Regex::new("").expect("regex pattern should be valid")
        ));
        assert!(!span_matches_tag_regex(
            &span,
            "service",
            &Regex::new(r"api-.*").expect("regex pattern should be valid")
        ));
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

        assert!(span_matches_tag_regex(
            &span,
            "env",
            &Regex::new(r"test.*").expect("regex pattern should be valid")
        ));
        assert!(!span_matches_tag_regex(
            &span,
            "env",
            &Regex::new(r"prod.*").expect("regex pattern should be valid")
        ));
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
        assert!(!filter_span_by_tags(
            &span_match_all,
            &TagFilters::from_config(&config)
        ));

        // Span that matches only one of the require tags
        let mut span_partial_match = Span::default();
        span_partial_match
            .meta
            .insert("env".to_string(), "production".to_string());
        assert!(filter_span_by_tags(
            &span_partial_match,
            &TagFilters::from_config(&config)
        ));

        // Span that doesn't match any require tags
        let mut span_no_match = Span::default();
        span_no_match
            .meta
            .insert("env".to_string(), "development".to_string());
        assert!(filter_span_by_tags(
            &span_no_match,
            &TagFilters::from_config(&config)
        ));
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
        assert!(filter_span_by_tags(
            &span_reject,
            &TagFilters::from_config(&config)
        ));

        // Span that doesn't match any reject tags
        let mut span_keep = Span::default();
        span_keep
            .meta
            .insert("env".to_string(), "production".to_string());
        assert!(!filter_span_by_tags(
            &span_keep,
            &TagFilters::from_config(&config)
        ));
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
        assert!(filter_span_by_tags(
            &span_both,
            &TagFilters::from_config(&config)
        ));

        // Span that matches require and doesn't match reject
        let mut span_good = Span::default();
        span_good
            .meta
            .insert("env".to_string(), "production".to_string());
        assert!(!filter_span_by_tags(
            &span_good,
            &TagFilters::from_config(&config)
        ));
    }

    #[test]
    fn test_compile_regex_filters() {
        let filters = vec![
            "env".to_string(),
            "service:^api-.*$".to_string(),
            "debug:[unclosed".to_string(),
        ];
        let compiled = compile_regex_filters(Some(filters.as_slice()));

        assert_eq!(compiled.len(), 2);
        assert!(matches!(&compiled[0], RegexFilter { key, regex: None } if key == "env"));
        assert!(matches!(
            &compiled[1],
            RegexFilter { key, regex: Some(r) }
                if key == "service" && r.as_str() == "^api-.*$"
        ));
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
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
            tags_provider: Arc::new(Provider::new(
                config.clone(),
                "lambda".to_string(),
                &std::collections::HashMap::from([(
                    "function_arn".to_string(),
                    "test-arn".to_string(),
                )]),
            )),
            span_pointers: None,
            tag_filters: Arc::new(TagFilters::from_config(config.as_ref())),
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
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
            tags_provider: Arc::new(Provider::new(
                config.clone(),
                "lambda".to_string(),
                &std::collections::HashMap::from([(
                    "function_arn".to_string(),
                    "test-arn".to_string(),
                )]),
            )),
            span_pointers: None,
            tag_filters: Arc::new(TagFilters::from_config(config.as_ref())),
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
            obfuscation_config: Arc::new(
                ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
            ),
            tags_provider: Arc::new(Provider::new(
                config.clone(),
                "lambda".to_string(),
                &std::collections::HashMap::from([(
                    "function_arn".to_string(),
                    "test-arn".to_string(),
                )]),
            )),
            span_pointers: None,
            tag_filters: Arc::new(TagFilters::from_config(config.as_ref())),
        };

        processor.process(&mut chunk, 0);

        assert_eq!(
            chunk.spans.len(),
            2,
            "Trace should be kept when no filter tags are set."
        );
    }

    /// Verifies that when `compute_trace_stats_on_extension` is true, `process_traces`
    /// filters sampled-out chunks from the backend payload while preserving them in the
    /// stats collection.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_process_traces_filters_sampled_out_chunks() {
        use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;

        let config = Arc::new(Config {
            apm_dd_url: "https://trace.agent.datadoghq.com".to_string(),
            compute_trace_stats_on_extension: true,
            ..Config::default()
        });
        let tags_provider = Arc::new(Provider::new(
            config.clone(),
            "lambda".to_string(),
            &std::collections::HashMap::from([(
                "function_arn".to_string(),
                "test-arn".to_string(),
            )]),
        ));
        let processor = ServerlessTraceProcessor::new(
            config.as_ref(),
            ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
        );

        let header_tags = tracer_header_tags::TracerHeaderTags {
            lang: "rust",
            lang_version: "1.0",
            lang_interpreter: "",
            lang_vendor: "",
            tracer_version: "1.0",
            container_id: "",
            client_computed_top_level: false,
            client_computed_stats: false,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };

        let make_span = |trace_id: u64, priority: Option<f64>| -> pb::Span {
            let mut metrics = HashMap::new();
            if let Some(p) = priority {
                metrics.insert("_sampling_priority_v1".to_string(), p);
            }
            pb::Span {
                trace_id,
                span_id: trace_id,
                parent_id: 0,
                metrics,
                service: "svc".to_string(),
                name: "op".to_string(),
                resource: "res".to_string(),
                ..Default::default()
            }
        };

        // Three traces: kept (priority 1), dropped (priority 0), dropped (priority -1)
        let traces = vec![
            vec![make_span(1, Some(1.0))],
            vec![make_span(2, Some(0.0))],
            vec![make_span(3, Some(-1.0))],
        ];

        let (payload_info, stats_collection) =
            processor.process_traces(config, tags_provider, header_tags, traces, 0, None);
        let payload_info = payload_info.expect("expected Some payload");

        // Stats collection must include all three traces
        let TracerPayloadCollection::V07(ref stats_payloads) = stats_collection else {
            panic!("expected V07");
        };
        let stats_span_count: usize = stats_payloads
            .iter()
            .flat_map(|tp| tp.chunks.iter())
            .map(|c| c.spans.len())
            .sum();
        assert_eq!(stats_span_count, 3, "stats must include all traces");

        // Backend payload must only contain the kept trace (priority 1)
        let backend_send_data = payload_info.builder.build();
        let TracerPayloadCollection::V07(backend_payloads) = backend_send_data.get_payloads()
        else {
            panic!("expected V07");
        };
        let backend_span_count: usize = backend_payloads
            .iter()
            .flat_map(|tp| tp.chunks.iter())
            .map(|c| c.spans.len())
            .sum();
        assert_eq!(
            backend_span_count, 1,
            "backend must only include kept traces"
        );
    }

    /// Verifies that `process_traces` returns `None` for the backend payload when all
    /// traces are sampled out and `compute_trace_stats_on_extension` is true.
    #[test]
    fn test_process_traces_returns_none_when_all_sampled_out() {
        use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;

        let config = Arc::new(Config {
            apm_dd_url: "https://trace.agent.datadoghq.com".to_string(),
            compute_trace_stats_on_extension: true,
            ..Config::default()
        });
        let tags_provider = Arc::new(Provider::new(
            config.clone(),
            "lambda".to_string(),
            &std::collections::HashMap::from([(
                "function_arn".to_string(),
                "test-arn".to_string(),
            )]),
        ));
        let processor = ServerlessTraceProcessor::new(
            config.as_ref(),
            ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
        );
        let header_tags = tracer_header_tags::TracerHeaderTags {
            lang: "rust",
            lang_version: "1.0",
            lang_interpreter: "",
            lang_vendor: "",
            tracer_version: "1.0",
            container_id: "",
            client_computed_top_level: false,
            client_computed_stats: false,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };

        let make_dropped_span = |trace_id: u64| -> pb::Span {
            let mut metrics = HashMap::new();
            metrics.insert("_sampling_priority_v1".to_string(), -1.0_f64);
            pb::Span {
                trace_id,
                span_id: trace_id,
                parent_id: 0,
                metrics,
                service: "svc".to_string(),
                name: "op".to_string(),
                resource: "res".to_string(),
                ..Default::default()
            }
        };

        let traces = vec![vec![make_dropped_span(1)], vec![make_dropped_span(2)]];

        let (payload, stats_collection) =
            processor.process_traces(config, tags_provider, header_tags, traces, 0, None);

        assert!(
            payload.is_none(),
            "backend payload must be None when all traces are sampled out"
        );

        // Stats collection must still include both traces
        let TracerPayloadCollection::V07(ref stats_payloads) = stats_collection else {
            panic!("expected V07");
        };
        let stats_span_count: usize = stats_payloads
            .iter()
            .flat_map(|tp| tp.chunks.iter())
            .map(|c| c.spans.len())
            .sum();
        assert_eq!(
            stats_span_count, 2,
            "stats must include all traces even when all are sampled out"
        );
    }

    /// Verifies that `body_size` in the returned `SendDataBuilderInfo` reflects the
    /// protobuf-encoded size of the filtered payload, not the original request body.
    #[test]
    fn test_process_traces_body_size_reflects_filtered_payload() {
        use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
        use prost::Message as _;

        let config = Arc::new(Config {
            apm_dd_url: "https://trace.agent.datadoghq.com".to_string(),
            compute_trace_stats_on_extension: true,
            ..Config::default()
        });
        let tags_provider = Arc::new(Provider::new(
            config.clone(),
            "lambda".to_string(),
            &std::collections::HashMap::from([(
                "function_arn".to_string(),
                "test-arn".to_string(),
            )]),
        ));
        let processor = ServerlessTraceProcessor::new(
            config.as_ref(),
            ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
        );
        let header_tags = tracer_header_tags::TracerHeaderTags {
            lang: "rust",
            lang_version: "1.0",
            lang_interpreter: "",
            lang_vendor: "",
            tracer_version: "1.0",
            container_id: "",
            client_computed_top_level: false,
            client_computed_stats: false,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };

        let make_span = |trace_id: u64, priority: f64| -> pb::Span {
            let mut metrics = HashMap::new();
            metrics.insert("_sampling_priority_v1".to_string(), priority);
            pb::Span {
                trace_id,
                span_id: trace_id,
                parent_id: 0,
                metrics,
                service: "svc".to_string(),
                name: "op".to_string(),
                resource: "res".to_string(),
                ..Default::default()
            }
        };

        // 1 kept trace, 3 dropped traces; original body_size is intentionally large
        let traces = vec![
            vec![make_span(1, 1.0)],
            vec![make_span(2, -1.0)],
            vec![make_span(3, -1.0)],
            vec![make_span(4, -1.0)],
        ];

        let (payload_info, stats_collection) =
            processor.process_traces(config, tags_provider, header_tags, traces, 999_999, None);

        let info = payload_info.expect("expected Some payload");

        // The reported size must equal the sum of encoded_len() of the kept TracerPayloads.
        // stats_collection has all 4 traces. Reconstruct the filtered payload (only trace_id=1
        // was kept with priority=1) and compute its encoded_len.
        let TracerPayloadCollection::V07(ref all_payloads) = stats_collection else {
            panic!("expected V07");
        };
        let expected_size: usize = all_payloads
            .iter()
            .filter_map(|tp| {
                let kept_chunks: Vec<pb::TraceChunk> = tp
                    .chunks
                    .iter()
                    .filter(|c| c.spans.iter().any(|s| s.trace_id == 1))
                    .cloned()
                    .collect();
                if kept_chunks.is_empty() {
                    None
                } else {
                    Some(pb::TracerPayload {
                        chunks: kept_chunks,
                        ..tp.clone()
                    })
                }
            })
            .map(|tp| tp.encoded_len())
            .sum();

        assert!(
            expected_size > 0,
            "expected_size must be non-zero for a non-empty payload"
        );
        assert_eq!(
            info.size, expected_size,
            "body_size must equal protobuf encoded_len of the filtered payload"
        );
        assert!(
            info.size < 999_999,
            "body_size must be smaller than the original unfiltered request size"
        );
    }

    /// Shared helper for the four `_dd.compute_stats` / stats-generation combination tests.
    ///
    /// Asserts:
    /// - `_dd.compute_stats` tag value in the trace payload
    /// - whether the extension generates stats via `send_processed_traces`
    ///
    /// | Input: `compute_trace_stats_on_extension` | Input: `client_computed_stats` | Expected: `_dd.compute_stats` | Expected: Extension generates stats? |
    /// |-------------------------------------------|--------------------------------|-------------------------------|--------------------------------------|
    /// | `false`                                   | `false`                        | `"1"`                         | No                                   |
    /// | `false`                                   | `true`                         | `"0"`                         | No                                   |
    /// | `true`                                    | `false`                        | `"0"`                         | Yes                                  |
    /// | `true`                                    | `true`                         | `"0"`                         | No                                   |
    #[allow(clippy::unwrap_used)]
    #[allow(clippy::too_many_lines)]
    async fn check_compute_stats_behavior(
        compute_trace_stats_on_extension: bool,
        client_computed_stats: bool,
    ) {
        use crate::traces::stats_concentrator_service::StatsConcentratorHandle;
        use crate::traces::stats_generator::StatsGenerator;
        use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
        use tokio::sync::mpsc;

        // "_dd.compute_stats" is "1" only when neither side computes stats (backend must do it).
        let expected_tag = if !compute_trace_stats_on_extension && !client_computed_stats {
            "1"
        } else {
            "0"
        };
        // The extension generates stats only when it is configured to do so and the tracer hasn't.
        let expect_stats = compute_trace_stats_on_extension && !client_computed_stats;

        let config = Arc::new(Config {
            apm_dd_url: "https://trace.agent.datadoghq.com".to_string(),
            compute_trace_stats_on_extension,
            ..Config::default()
        });
        let tags_provider = Arc::new(Provider::new(
            config.clone(),
            "lambda".to_string(),
            &std::collections::HashMap::from([(
                "function_arn".to_string(),
                "test-arn".to_string(),
            )]),
        ));
        let processor = Arc::new(ServerlessTraceProcessor::new(
            config.as_ref(),
            ObfuscationConfig::new().expect("Failed to create ObfuscationConfig"),
        ));
        let header_tags = tracer_header_tags::TracerHeaderTags {
            lang: "rust",
            lang_version: "1.0",
            lang_interpreter: "",
            lang_vendor: "",
            tracer_version: "1.0",
            container_id: "",
            client_computed_top_level: false,
            client_computed_stats,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };
        let span = pb::Span {
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            service: "svc".to_string(),
            name: "op".to_string(),
            resource: "res".to_string(),
            ..Default::default()
        };

        let (stats_tx, mut stats_rx) = mpsc::unbounded_channel();
        let stats_handle = StatsConcentratorHandle::new(stats_tx);
        let stats_generator = Arc::new(StatsGenerator::new(stats_handle));
        let (trace_tx, mut trace_rx) = mpsc::channel(10);
        let sending_processor = SendingTraceProcessor {
            appsec: None,
            processor,
            trace_tx,
            stats_generator,
        };

        sending_processor
            .send_processed_traces(
                config,
                tags_provider,
                header_tags,
                vec![vec![span]],
                0,
                None,
            )
            .await
            .expect("send_processed_traces should not fail in tests");

        if expect_stats {
            assert!(
                stats_rx.try_recv().is_ok(),
                "extension must generate stats when compute_trace_stats_on_extension=true \
                 and client_computed_stats=false"
            );
        } else {
            assert!(
                stats_rx.try_recv().is_err(),
                "extension must not generate stats (compute_trace_stats_on_extension={compute_trace_stats_on_extension}, \
                 client_computed_stats={client_computed_stats})"
            );
        }

        let payload_info = trace_rx
            .try_recv()
            .expect("expected payload in trace_tx channel");
        let send_data = payload_info.builder.build();
        let TracerPayloadCollection::V07(payloads) = send_data.get_payloads() else {
            panic!("expected V07");
        };
        for payload in payloads {
            assert_eq!(
                payload
                    .tags
                    .get(crate::tags::lambda::tags::COMPUTE_STATS_KEY),
                Some(&expected_tag.to_string()),
                "_dd.compute_stats must be {expected_tag} (compute_trace_stats_on_extension={compute_trace_stats_on_extension}, \
                 client_computed_stats={client_computed_stats})"
            );
        }
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_compute_stats_tag_neither_side_computes() {
        // Neither extension nor tracer computes stats → backend must compute → tag "1".
        check_compute_stats_behavior(false, false).await;
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_compute_stats_tag_tracer_computes() {
        // Tracer computed stats (Datadog-Client-Computed-Stats header set) → tag "0".
        check_compute_stats_behavior(false, true).await;
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_compute_stats_tag_extension_computes() {
        // Extension computes stats (DD_COMPUTE_TRACE_STATS_ON_EXTENSION=true) → tag "0".
        check_compute_stats_behavior(true, false).await;
    }

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_compute_stats_tag_both_compute() {
        // Both extension and tracer compute stats → tracer takes precedence, tag "0", no double-count.
        check_compute_stats_behavior(true, true).await;
    }
}
