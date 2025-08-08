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
use datadog_trace_obfuscation::obfuscate::obfuscate_span;
use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_protobuf::pb;
use datadog_trace_protobuf::pb::Span;
use datadog_trace_utils::send_data::{Compression, SendDataBuilder};
use datadog_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
use datadog_trace_utils::trace_utils::{self};
use datadog_trace_utils::tracer_header_tags;
use datadog_trace_utils::tracer_payload::{TraceChunkProcessor, TracerPayloadCollection};
use ddcommon::Endpoint;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::error::SendError;
use tracing::{debug, error};

use super::trace_aggregator::SendDataBuilderInfo;

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceProcessor {
    pub obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
}

struct ChunkProcessor {
    obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    tags_provider: Arc<provider::Provider>,
    span_pointers: Option<Vec<SpanPointer>>,
}

impl TraceChunkProcessor for ChunkProcessor {
    fn process(&mut self, chunk: &mut pb::TraceChunk, root_span_index: usize) {
        chunk
            .spans
            .retain(|span| !filter_span_from_lambda_library_or_runtime(span));
        for span in &mut chunk.spans {
            // Service name could be incorrectly set to 'aws.lambda'
            // in datadog lambda libraries
            if span.service == "aws.lambda" {
                if let Some(service) = self.tags_provider.get_tags_map().get("service") {
                    span.service.clone_from(service);
                }
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

fn filter_span_from_lambda_library_or_runtime(span: &Span) -> bool {
    if let Some(url) = span.meta.get("http.url") {
        if url.starts_with(LAMBDA_RUNTIME_URL_PREFIX)
            || url.starts_with(LAMBDA_EXTENSION_URL_PREFIX)
            || url.starts_with(LAMBDA_STATSD_URL_PREFIX)
        {
            return true;
        }
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

    if let Some(dns_address) = span.meta.get("dns.address") {
        if dns_address.starts_with(DNS_NON_ROUTABLE_ADDRESS_URL_PREFIX)
            || dns_address.starts_with(DNS_LOCAL_HOST_ADDRESS_URL_PREFIX)
            || dns_address.starts_with(AWS_XRAY_DAEMON_ADDRESS_URL_PREFIX)
        {
            return true;
        }
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
    async fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags<'_>,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        span_pointers: Option<Vec<SpanPointer>>,
    ) -> SendDataBuilderInfo;
}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    async fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags<'_>,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        span_pointers: Option<Vec<SpanPointer>>,
    ) -> SendDataBuilderInfo {
        let mut payload = trace_utils::collect_pb_trace_chunks(
            traces,
            &header_tags,
            &mut ChunkProcessor {
                obfuscation_config: self.obfuscation_config.clone(),
                tags_provider: tags_provider.clone(),
                span_pointers,
            },
            true, // send agentless since we are the agent
        )
        .unwrap_or_else(|e| {
            error!("Error processing traces: {:?}", e);
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
        };

        let builder = SendDataBuilder::new(body_size, payload, header_tags, &endpoint)
            .with_compression(Compression::Zstd(6))
            .with_retry_strategy(RetryStrategy::new(
                1,
                100,
                RetryBackoffType::Exponential,
                None,
            ));

        SendDataBuilderInfo::new(builder, body_size)
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
                    debug!("TRACE_PROCESSOR | holding trace for App & API Protection additional data");
                    ctx.hold_trace(trace, SendingTraceProcessor{ appsec:  None, processor: self.processor.clone(), trace_tx: self.trace_tx.clone() }, HoldArguments{
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

        let payload = self
            .processor
            .process_traces(
                config,
                tags_provider,
                header_tags,
                traces,
                body_size,
                span_pointers,
            )
            .await;
        self.trace_tx.send(payload).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        time::{SystemTime, UNIX_EPOCH},
    };

    use datadog_trace_obfuscation::obfuscation_config::ObfuscationConfig;

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
            obfuscation_config: Arc::new(ObfuscationConfig::new().unwrap()),
        };
        let config = create_test_config();
        let tags_provider = create_tags_provider(config.clone());
        let tracer_payload = trace_processor.process_traces(
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
            tracer_payload.await.builder.build().get_payloads()
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
}
