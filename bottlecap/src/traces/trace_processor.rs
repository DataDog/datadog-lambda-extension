// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::tags::provider;
use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_utils::config_utils::trace_intake_url;
use datadog_trace_utils::tracer_payload::TraceEncoding;
use ddcommon::Endpoint;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use hyper::{http, Body, Request, Response, StatusCode};
use tokio::sync::mpsc::Sender;
use tracing::info;

use crate::config;
use crate::traces::http_utils::{self, log_and_create_http_response};
use datadog_trace_obfuscation::obfuscate::obfuscate_span;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::trace_utils::{self};

use super::trace_agent::{ApiVersion, MAX_CONTENT_LENGTH};

#[async_trait]
pub trait TraceProcessor {
    /// Deserializes traces from a hyper request body and sends them through the provided tokio mpsc
    /// Sender.
    async fn process_traces(
        &self,
        config: Arc<config::Config>,
        req: Request<Body>,
        tx: Sender<trace_utils::SendData>,
        tags_provider: Arc<provider::Provider>,
        version: ApiVersion,
    ) -> http::Result<Response<Body>>;
}
#[allow(clippy::module_name_repetitions)]
#[derive(Clone)]
pub struct ServerlessTraceProcessor {
    pub obfuscation_config: Arc<obfuscation_config::ObfuscationConfig>,
}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    async fn process_traces(
        &self,
        config: Arc<config::Config>,
        req: Request<Body>,
        tx: Sender<trace_utils::SendData>,
        tags_provider: Arc<provider::Provider>,
        version: ApiVersion,
    ) -> http::Result<Response<Body>> {
        info!("Recieved traces to process");
        let (parts, body) = req.into_parts();

        if let Some(response) = http_utils::verify_request_content_length(
            &parts.headers,
            MAX_CONTENT_LENGTH,
            "Error processing traces",
        ) {
            return response;
        }

        let tracer_header_tags = (&parts.headers).into();

        // deserialize traces from the request body, convert to protobuf structs (see trace-protobuf
        // crate)
        let (body_size, traces) = match version {
            ApiVersion::V04 => match trace_utils::get_traces_from_request_body(body).await {
                Ok(result) => result,
                Err(err) => {
                    return log_and_create_http_response(
                        &format!("Error deserializing trace from request body: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            },
            ApiVersion::V05 => match trace_utils::get_v05_traces_from_request_body(body).await {
                Ok(result) => result,
                Err(err) => {
                    return log_and_create_http_response(
                        &format!("Error deserializing trace from request body: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            },
        };

        let payload = trace_utils::collect_trace_chunks(
            traces,
            &tracer_header_tags,
            |chunk, _root_span_index| {
                // trace_utils::set_serverless_root_span_tags(
                //     &mut chunk.spans[root_span_index],
                //     config.function_name.clone(),
                //     &config.env_type,
                // );
                chunk.spans.retain(|span| {
                    (span.name != "dns.lookup" && span.resource != "0.0.0.0")
                        || (span.name != "dns.lookup" && span.resource != "127.0.0.1")
                });
                for span in &mut chunk.spans {
                    tags_provider.get_tags_map().iter().for_each(|(k, v)| {
                        span.meta.insert(k.clone(), v.clone());
                    });
                    // TODO(astuyve) generalize this and delegate to an enum
                    span.meta.insert("origin".to_string(), "lambda".to_string());
                    span.meta
                        .insert("_dd.origin".to_string(), "lambda".to_string());
                    obfuscate_span(span, &self.obfuscation_config);
                }
            },
            true,
            TraceEncoding::V07,
        );
        let intake_url = trace_intake_url(&config.site);
        let endpoint = Endpoint {
            url: hyper::Uri::from_str(&intake_url).unwrap(),
            api_key: Some(config.api_key.clone().into()),
        };

        let send_data = SendData::new(body_size, payload, tracer_header_tags, &endpoint);

        // send trace payload to our trace flusher
        match tx.send(send_data).await {
            Ok(()) => {
                return log_and_create_http_response(
                    "Successfully buffered traces to be flushed.",
                    StatusCode::ACCEPTED,
                );
            }
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error sending traces to the trace flusher: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_obfuscation::obfuscation_config::ObfuscationConfig;
    use hyper::Request;
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::sync::mpsc::{self, Receiver, Sender};

    use crate::config::Config;
    use crate::tags::provider::Provider;
    use crate::traces::trace_processor::{self, TraceProcessor};
    use crate::LAMBDA_RUNTIME_SLUG;
    use datadog_trace_protobuf::pb;
    use datadog_trace_utils::{trace_utils, tracer_payload::TracerPayloadCollection};

    fn get_current_timestamp_nanos() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
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
    #[cfg_attr(miri, ignore)]
    async fn test_process_trace() {
        let (tx, mut rx): (
            Sender<trace_utils::SendData>,
            Receiver<trace_utils::SendData>,
        ) = mpsc::channel(1);

        let start = get_current_timestamp_nanos();

        let json_span = create_test_json_span(11, 222, 333, start);

        let bytes = rmp_serde::to_vec(&vec![vec![json_span]]).unwrap();
        let request = Request::builder()
            .header("datadog-meta-tracer-version", "4.0.0")
            .header("datadog-meta-lang", "nodejs")
            .header("datadog-meta-lang-version", "v19.7.0")
            .header("datadog-meta-lang-interpreter", "v8")
            .header("datadog-container-id", "33")
            .header("content-length", "100")
            .body(hyper::body::Body::from(bytes))
            .unwrap();

        let trace_processor = trace_processor::ServerlessTraceProcessor {
            obfuscation_config: Arc::new(ObfuscationConfig::new().unwrap()),
        };
        let config = create_test_config();
        let tags_provider = create_tags_provider(config.clone());
        let res = trace_processor
            .process_traces(
                config,
                request,
                tx,
                tags_provider.clone(),
                crate::traces::trace_agent::ApiVersion::V04,
            )
            .await;
        assert!(res.is_ok());

        let tracer_payload = rx.recv().await;

        assert!(tracer_payload.is_some());

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
            if let TracerPayloadCollection::V07(payload) = tracer_payload.unwrap().get_payloads() {
                Some(payload[0].clone())
            } else {
                None
            };

        assert_eq!(expected_tracer_payload, received_payload.unwrap());
    }
}
