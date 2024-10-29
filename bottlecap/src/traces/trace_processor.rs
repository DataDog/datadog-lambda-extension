// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::config_utils::trace_intake_url;
use datadog_trace_utils::tracer_header_tags;
use datadog_trace_utils::tracer_payload::{TraceChunkProcessor, TraceCollection::V07};
use ddcommon::Endpoint;
use std::str::FromStr;
use std::sync::Arc;

use tracing::debug;

use crate::config;
use crate::tags::provider::Provider;
use datadog_trace_obfuscation::obfuscate::obfuscate_span;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::trace_utils::{self};

#[derive(Clone)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessTraceProcessor {
    pub obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    pub resolved_api_key: String,
    pub override_trace_id: Option<u64>,
    pub aws_lambda_span_id: Option<u64>,
    pub root_parent_id: Option<u64>,
}

struct ChunkProcessor {
    obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
    tags_provider: Arc<Provider>,
    override_trace_id: Option<u64>,
    override_span_id: Option<u64>,
    substitute_parent_span_id: Option<(u64, u64)>,
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

            if self.substitute_parent_span_id.is_some()
                && self.substitute_parent_span_id.unwrap().0 == span.parent_id
            {
                span.parent_id = self.substitute_parent_span_id.unwrap().1;
            }
            if self.override_trace_id.is_some() {
                span.trace_id = self.override_trace_id.unwrap();
            }
            if self.override_span_id.is_some() {
                span.span_id = self.override_span_id.unwrap();
            }
        }
    }
}

#[allow(clippy::module_name_repetitions)]
pub trait TraceProcessor {
    fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        aws_lambda_span: bool,
    ) -> SendData;
    fn override_ids(&mut self, trace_id: u64, parent_id: u64, aws_lambda_span_id: u64);
}

impl TraceProcessor for ServerlessTraceProcessor {
    fn process_traces(
        &self,
        config: Arc<config::Config>,
        tags_provider: Arc<Provider>,
        header_tags: tracer_header_tags::TracerHeaderTags,
        traces: Vec<Vec<pb::Span>>,
        body_size: usize,
        aws_lambda_span: bool,
    ) -> SendData {
        debug!("Received traces to process");

        let payload = trace_utils::collect_trace_chunks(
            V07(traces),
            &header_tags,
            &mut self.configure_chunk_processor(tags_provider, aws_lambda_span),
            true,
        );
        let intake_url = trace_intake_url(&config.site);
        let endpoint = Endpoint {
            url: hyper::Uri::from_str(&intake_url).expect("can't parse trace intake URL, exiting"),
            api_key: Some(self.resolved_api_key.clone().into()),
            timeout_ms: Endpoint::DEFAULT_TIMEOUT,
            test_token: None,
        };

        SendData::new(
            body_size,
            payload,
            header_tags,
            &endpoint,
            config.https_proxy.clone(),
        )
    }

    fn override_ids(&mut self, trace_id: u64, parent_id: u64, aws_lambda_span_id: u64) {
        self.override_trace_id = Some(trace_id);
        self.root_parent_id = Some(parent_id);
        self.aws_lambda_span_id = Some(aws_lambda_span_id);
    }
}

impl ServerlessTraceProcessor {
    fn configure_chunk_processor(
        &self,
        tags_provider: Arc<Provider>,
        aws_lambda_span: bool,
    ) -> ChunkProcessor {
        // Supports overriding of trace id and re-parenting, currently used for LWA
        // if LWA is not used, `override_ids` is not called and `override_trace_id` and `substitute_parent_span_id` are None. No substitution should be done
        // if LWA is used and `override_ids` is called
        // - `override_trace_id` can be present and if found, it should override all trace_ids of all spans
        // - `substitute_parent_span_id` will be used to reparent parent_found -> substitute_parent

        // spans are coming from 2 sources:
        // - the tracer, which will need to overwrite the trace_id if available, and its root-level span should be re-parented with
        // the aws.lambda span if it is 0 or if it has a parent_id from headers that was captured in self.substitute_parent_span_id
        // - the aws.lambda processor span, which will need to get the trace_id if available and re-parented only if there was
        // a substitute_parent_span_id. Inferred span

        let override_parent_with_lambda_span =
            if !aws_lambda_span && self.aws_lambda_span_id.is_some() {
                Some((
                    self.root_parent_id.unwrap_or(0),
                    self.aws_lambda_span_id.unwrap(),
                ))
            } else {
                None
            };

        let span_id_override = if aws_lambda_span {
            self.aws_lambda_span_id
        } else {
            None
        };

        ChunkProcessor {
            obfuscation_config: self.obfuscation_config.clone(),
            tags_provider: tags_provider.clone(),
            override_trace_id: self.override_trace_id,
            substitute_parent_span_id: override_parent_with_lambda_span,
            override_span_id: span_id_override,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::tags::provider::Provider;
    use crate::traces::trace_processor;
    use crate::traces::trace_processor::TraceProcessor;
    use crate::LAMBDA_RUNTIME_SLUG;
    use datadog_trace_obfuscation::obfuscation_config::ObfuscationConfig;
    use datadog_trace_protobuf::pb;
    use datadog_trace_utils::{tracer_header_tags, tracer_payload::TracerPayloadCollection};
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

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
        };

        let trace_processor = trace_processor::ServerlessTraceProcessor {
            resolved_api_key: "foo".to_string(),
            obfuscation_config: Arc::new(ObfuscationConfig::new().unwrap()),
            override_trace_id: None,
            root_parent_id: None,
            aws_lambda_span_id: None,
        };
        let config = create_test_config();
        let tags_provider = create_tags_provider(config.clone());
        let tracer_payload = trace_processor.process_traces(
            config,
            tags_provider.clone(),
            header_tags,
            traces,
            100,
            false,
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
