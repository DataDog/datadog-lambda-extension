// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::config;
use crate::tags::provider;
use crate::traces::span_pointers::{attach_span_pointers_to_meta, SpanPointer};
use crate::traces::{
    AWS_XRAY_DAEMON_ADDRESS_URL_PREFIX, DNS_LOCAL_HOST_ADDRESS_URL_PREFIX,
    DNS_NON_ROUTABLE_ADDRESS_URL_PREFIX, INVOCATION_SPAN_RESOURCE, LAMBDA_EXTENSION_URL_PREFIX,
    LAMBDA_RUNTIME_URL_PREFIX, LAMBDA_STATSD_URL_PREFIX,
};
use datadog_trace_obfuscation::obfuscate::obfuscate_span;
use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_protobuf::pb;
use datadog_trace_protobuf::pb::Span;
use datadog_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::trace_utils::{self};
use datadog_trace_utils::tracer_header_tags;
use datadog_trace_utils::tracer_payload::{
    TraceChunkProcessor, TraceCollection::V07, TracerPayloadCollection,
};
use ddcommon::Endpoint;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceProcessor {
    pub obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    pub resolved_api_key: String,
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
pub trait TraceProcessor {
    fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        span_pointers: Option<Vec<SpanPointer>>,
    ) -> SendData;
}

impl TraceProcessor for ServerlessTraceProcessor {
    fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        span_pointers: Option<Vec<SpanPointer>>,
    ) -> SendData {
        let mut payload = trace_utils::collect_trace_chunks(
            V07(traces),
            &header_tags,
            &mut ChunkProcessor {
                obfuscation_config: self.obfuscation_config.clone(),
                tags_provider: tags_provider.clone(),
                span_pointers,
            },
            true,
            false,
        ).unwrap_or_else(|e| {
            log::error!("Error processing traces: {:?}", e);
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
            url: hyper::Uri::from_str(&config.apm_config_apm_dd_url)
                .expect("can't parse trace intake URL, exiting"),
            api_key: Some(self.resolved_api_key.clone().into()),
            timeout_ms: config.flush_timeout * 1_000,
            test_token: None,
        };

        let mut send_data = SendData::new(body_size, payload, header_tags, &endpoint);
        send_data.set_retry_strategy(RetryStrategy::new(
            1,
            100,
            RetryBackoffType::Exponential,
            None,
        ));
        send_data
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_obfuscation::obfuscation_config::ObfuscationConfig;
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::config::Config;
    use crate::tags::provider::Provider;
    use crate::traces::trace_processor::{self, TraceProcessor};
    use crate::LAMBDA_RUNTIME_SLUG;
    use datadog_trace_protobuf::pb;
    use datadog_trace_utils::{tracer_header_tags, tracer_payload::TracerPayloadCollection};

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
            apm_config_apm_dd_url: "https://trace.agent.datadoghq.com".to_string(),
            service: Some("test-service".to_string()),
            tags: Some("test:tag,env:test-env".to_string()),
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

        let trace_processor = trace_processor::ServerlessTraceProcessor {
            resolved_api_key: "foo".to_string(),
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

        let received_payload =
            if let TracerPayloadCollection::V07(payload) = tracer_payload.get_payloads() {
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
