// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::tags::provider;
use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::config_utils::trace_intake_url;
use datadog_trace_utils::tracer_header_tags;
use datadog_trace_utils::tracer_payload::{TraceChunkProcessor, TraceEncoding};
use ddcommon::Endpoint;
use std::str::FromStr;
use std::sync::Arc;

use tracing::debug;

use crate::config;
use datadog_trace_obfuscation::obfuscate::obfuscate_span;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::trace_utils::{self};

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceProcessor {
    pub obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    pub resolved_api_key: String,
}

struct ChunkProcessor {
    obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    tags_provider: Arc<provider::Provider>,
}

impl TraceChunkProcessor for ChunkProcessor {
    fn process(&mut self, chunk: &mut pb::TraceChunk, _index: usize) {
        chunk.spans.retain(|span| {
            (span.resource != "127.0.0.1" || span.resource != "0.0.0.0")
                && span.name != "dns.lookup"
        });
        for span in &mut chunk.spans {
            self.tags_provider.get_tags_map().iter().for_each(|(k, v)| {
                span.meta.insert(k.clone(), v.clone());
            });
            // TODO(astuyve) generalize this and delegate to an enum
            span.meta.insert("origin".to_string(), "lambda".to_string());
            span.meta
                .insert("_dd.origin".to_string(), "lambda".to_string());
            obfuscate_span(span, &self.obfuscation_config);
        }
    }
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
    ) -> SendData {
        debug!("Received traces to process");
        let payload = trace_utils::collect_trace_chunks(
            traces,
            &header_tags,
            &mut ChunkProcessor {
                obfuscation_config: self.obfuscation_config.clone(),
                tags_provider: tags_provider.clone(),
            },
            true,
            TraceEncoding::V07,
        );
        let intake_url = trace_intake_url(&config.site);
        let endpoint = Endpoint {
            url: hyper::Uri::from_str(&intake_url).expect("can't parse trace intake URL, exiting"),
            api_key: Some(self.resolved_api_key.clone().into()),
            timeout_ms: Endpoint::DEFAULT_TIMEOUT,
            test_token: None,
        };

        SendData::new(body_size, payload, header_tags, &endpoint)
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_obfuscation::obfuscation_config::ObfuscationConfig;
    use serde_json::json;
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
            service: Some("test-service".to_string()),
            tags: Some("test:tag,env:test".to_string()),
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

    fn create_test_json_span(
        trace_id: u64,
        span_id: u64,
        parent_id: u64,
        start: i64,
    ) -> serde_json::Value {
        json!(
            {
                "trace_id": trace_id,
                "span_id": span_id,
                "service": "test-service",
                "name": "test_name",
                "resource": "test-resource",
                "parent_id": parent_id,
                "start": start,
                "duration": 5,
                "error": 0,
                "meta": {
                    "service": "test-service",
                    "env": "test-env",
                    "runtime-id": "test-runtime-id-value",
                },
                "metrics": {},
                "meta_struct": {},
            }
        )
    }
    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    #[cfg_attr(miri, ignore)]
    async fn test_process_trace() {
        let start = get_current_timestamp_nanos();

        let json_span = create_test_json_span(11, 222, 333, start);

        let json_bytes = serde_json::to_vec(&json_span).expect("invalid json span");
        let span: pb::Span =
            rmp_serde::from_slice(&json_bytes).expect("couldnt convert to proto span");

        let traces: Vec<Vec<pb::Span>> = vec![vec![span]];

        let header_tags = tracer_header_tags::TracerHeaderTags {
            lang: "nodejs",
            lang_version: "v19.7.0",
            lang_interpreter: "v8",
            lang_vendor: "vendor",
            tracer_version: "4.0.0",
            container_id: "33",
            client_computed_top_level: false,
            client_computed_stats: false,
        };

        let trace_processor = trace_processor::ServerlessTraceProcessor {
            resolved_api_key: "foo".to_string(),
            obfuscation_config: Arc::new(ObfuscationConfig::new().unwrap()),
        };
        let config = create_test_config();
        let tags_provider = create_tags_provider(config.clone());
        let tracer_payload =
            trace_processor.process_traces(config, tags_provider.clone(), header_tags, traces, 100);

        let expected_tracer_payload = pb::TracerPayload {
            container_id: "33".to_string(),
            language_name: "nodejs".to_string(),
            language_version: "v19.7.0".to_string(),
            tracer_version: "4.0.0".to_string(),
            runtime_id: "test-runtime-id-value".to_string(),
            chunks: vec![pb::TraceChunk {
                priority: i32::from(i8::MIN),
                origin: String::new(),
                spans: vec![create_test_span(11, 222, 333, start, true, tags_provider)],
                tags: HashMap::new(),
                dropped_trace: false,
            }],
            tags: HashMap::new(),
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
