// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use rmp;

use async_trait::async_trait;
use hyper::{http, Body, Request, Response, body::Buf, StatusCode};
use rmpv::decode::read_value;
use rmpv::Value;
use rmp::decode::read_str_len;
use tokio::sync::mpsc::Sender;
use tracing::{debug, info};

use datadog_trace_obfuscation::obfuscate::obfuscate_span;
use datadog_trace_utils::trace_utils::SendData;
use datadog_trace_utils::trace_utils::{self};
use datadog_trace_protobuf::pb::{self, Span, TraceChunk};

use crate::traces::{
    config::Config as TraceConfig,
    http_utils::{self, log_and_create_http_response},
};

#[async_trait]
pub trait TraceProcessor {
    /// Deserializes traces from a hyper request body and sends them through the provided tokio mpsc
    /// Sender.
    async fn process_traces_v4(
        &self,
        config: Arc<TraceConfig>,
        req: Request<Body>,
        tx: Sender<trace_utils::SendData>,
    ) -> http::Result<Response<Body>>;

    async fn process_traces_v5(
        &self,
        config: Arc<TraceConfig>,
        req: Request<Body>,
        tx: Sender<trace_utils::SendData>,
    ) -> http::Result<Response<Body>>;
}

#[derive(Clone, Copy)]
pub struct ServerlessTraceProcessor {}

#[async_trait]
impl TraceProcessor for ServerlessTraceProcessor {
    async fn process_traces_v5(
        &self,
        config: Arc<TraceConfig>,
        req: Request<Body>,
        tx: Sender<trace_utils::SendData>,
    ) -> http::Result<Response<Body>> {
        info!("Recieved traces to process");
        let (parts, body) = req.into_parts();

        if let Some(response) = http_utils::verify_request_content_length(
            &parts.headers,
            config.max_request_content_length,
            "Error processing traces and verifying length",
        ) {
            return response;
        }

        println!("astuyve no error in verifying request content length");

        let tracer_header_tags = (&parts.headers).into();

        // deserialize traces from the request body, convert to protobuf structs (see trace-protobuf
        // crate)
        let buffer = hyper::body::aggregate(body).await.unwrap();
        let body_size = buffer.remaining();
        let mut reader = buffer.reader();
        let wrapper_size = rmp::decode::read_array_len(&mut reader).unwrap();
        assert!(wrapper_size == 2); //todo conver to http error/response
 
        // START read dict
        let dict_size = match rmp::decode::read_array_len(&mut reader) {
            Ok(res) => res,
            Err(err) => {
                println!("ASTUYVE error reading dict size: {err}");
                return log_and_create_http_response(
                    &format!("ASTUYVE Error reading dict size: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };
        let mut dict: Vec<String> = Default::default();
        for _ in 0..dict_size {
            let val: Value = read_value(&mut reader).unwrap();
            match val {
                Value::String(s) => {
                    // dict.push(s.to_string());
                    let s = s.to_string().replace(&['\"'][..], "");
                    dict.push(s);
                }
                _ => {
                    return log_and_create_http_response(
                        &format!("Value in string dict is not a string: {val}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            }
        }
        // START read traces

        let traces_size = match rmp::decode::read_array_len(&mut reader) {
            Ok(res) => res,
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error reading traces size: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };
        let mut traces: Vec<Vec<Span>> = Default::default();

        for _ in 0..traces_size {
            let spans_size = match rmp::decode::read_array_len(&mut reader) {
                Ok(res) => res,
                Err(err) => {
                    println!("ASTUYVE error reading spans size: {err}");
                    return log_and_create_http_response(
                        &format!("ASTUYVE Error reading spans size: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };
            let mut trace: Vec<Span> = Default::default();

            for _ in 0..spans_size {
                let mut span: Span = Default::default();
                let span_size = rmp::decode::read_array_len(&mut reader).unwrap();
                assert!(span_size == 12); //todo convert to http error/response
                //0 - service
                match read_value(&mut reader).unwrap() {
                    Value::Integer(s) => {
                        let string_id = s.as_i64().unwrap() as usize;
                        span.service = dict[string_id].to_string();
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span service is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                // 1 - name
                match read_value(&mut reader).unwrap() {
                    Value::Integer(s) => {
                        let string_id = s.as_i64().unwrap() as usize;
                        span.name = dict[string_id].to_string();
                        println!("ASTUYVE span name is {:?}", span.name);
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span name is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                // 2 - resource
                match read_value(&mut reader).unwrap() {
                    Value::Integer(s) => {
                        let string_id = s.as_i64().unwrap() as usize;
                        span.resource = dict[string_id].to_string();
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span resource is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                // 3 - trace_id
                match read_value(&mut reader).unwrap() {
                    Value::Integer(i) => {
                        span.trace_id = i.as_u64().unwrap();
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span resource is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                // 4 - span_id
                match read_value(&mut reader).unwrap() {
                    Value::Integer(i) => {
                        span.span_id = i.as_u64().unwrap();
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span span_id is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                // 5 - parent_id
                match read_value(&mut reader).unwrap() {
                    Value::Integer(i) => {
                        span.parent_id = i.as_u64().unwrap()
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span parent_id is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                //6 - start
                match read_value(&mut reader).unwrap() {
                    Value::Integer(i) => {
                        span.start = i.as_i64().unwrap()
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span start is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                //7 - duration
                match read_value(&mut reader).unwrap() {
                    Value::Integer(i) => {
                        span.duration = i.as_i64().unwrap()
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span duration is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                //8 - error
                match read_value(&mut reader).unwrap() {
                    Value::Integer(i) => {
                        span.error = i.as_i64().unwrap() as i32
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span error is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                }
                //9 - meta
                match read_value(&mut reader).unwrap() {
                    Value::Map(meta) => {
                        for (k, v) in meta.iter() {
                            match k {
                                Value::Integer(k) => {
                                    match v {
                                        Value::Integer(v) => {
                                            let key_id = k.as_i64().unwrap() as usize;
                                            let val_id = v.as_i64().unwrap() as usize;
                                            let key = dict[key_id].to_string();
                                            let val = dict[val_id].to_string();
                                            span.meta.insert(key, val);
                                        }
                                        _ => {
                                            return log_and_create_http_response(
                                                &format!("ASTUYVE Value in span meta value is not a string {v}"),
                                                StatusCode::INTERNAL_SERVER_ERROR,
                                            );
                                        }
                                    }
                                }
                                _ => {
                                    return log_and_create_http_response(
                                        &format!("ASTUYVE Value in span meta key is not a string {k}"),
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                    );
                                }
                            }
                        }
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span meta is not a map {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                }
                // 10 - metrics
                match read_value(&mut reader).unwrap() {
                    Value::Map(metrics) => {
                        for (k, v) in metrics.iter() {
                            match k {
                                Value::Integer(k) => {
                                    match v {
                                        Value::Integer(v) => {
                                            let key_id = k.as_i64().unwrap() as usize;
                                            let key = dict[key_id].to_string();
                                            span.metrics.insert(key, v.as_f64().unwrap());
                                        },
                                        Value::F64(v) => {
                                            let key_id = k.as_i64().unwrap() as usize;
                                            let key = dict[key_id].to_string();
                                            span.metrics.insert(key, *v);
                                        },
                                        _ => {
                                            return log_and_create_http_response(
                                                &format!("ASTUYVE Value in span metrics value is not a float {v}"),
                                                StatusCode::INTERNAL_SERVER_ERROR,
                                            );
                                        }
                                    }
                                }
                                _ => {
                                    return log_and_create_http_response(
                                        &format!("ASTUYVE Value in span metrics key is not a string {k}"),
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                    );
                                }
                            }
                        }
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span metrics is not a map {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                }

                // 11 - type
                match read_value(&mut reader).unwrap() {
                    Value::Integer(s) => {
                        let string_id = s.as_i64().unwrap() as usize;
                        span.r#type = dict[string_id].to_string();
                    },
                    val => {
                        return log_and_create_http_response(
                            &format!("ASTUYVE Value in span type is not a string {val}"),
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                }
                println!("ASTUYVE span is {:?}", span);
                trace.push(span);
            }
            traces.push(trace);
        }





        // let value: Value = read_value(&mut reader).unwrap();
        // println!("ASTUYVE rest of value after two read_array_len is {:?}", value);
        // let dict = &value.as_array().unwrap()[0];
        // println!("ASTUYVE string dict is {:?}", dict);
        // let compressed_traces = &value.as_array().unwrap()[1];
        // println!("ASTUYVE compressed traces is {:?}", compressed_traces);

        let payload = trace_utils::collect_trace_chunks(
            traces,
            &tracer_header_tags,
            |chunk, root_span_index| {
                trace_utils::set_serverless_root_span_tags(
                    &mut chunk.spans[root_span_index],
                    config.function_name.clone(),
                    &config.env_type,
                );
                chunk.spans.retain(|span| {
                    return (span.name != "dns.lookup" && span.resource != "0.0.0.0")
                        || (span.name != "dns.lookup" && span.resource != "127.0.0.1");
                });
                for span in chunk.spans.iter_mut() {
                    debug!("ASTUYVE span is {:?}", span);
                    // trace_utils::enrich_span_with_mini_agent_metadata(span, &mini_agent_metadata);
                    // trace_utils::enrich_span_with_azure_metadata(
                    //     span,
                    //     config.mini_agent_version.as_str(),
                    // );
                    obfuscate_span(span, &config.obfuscation_config);
                }
            },
            true, // In mini agent, we always send agentless
        );

        let send_data = SendData::new(body_size, payload, tracer_header_tags, &config.trace_intake);

        // send trace payload to our trace flusher
        match tx.send(send_data).await {
            Ok(_) => {
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

    async fn process_traces_v4(
        &self,
        config: Arc<TraceConfig>,
        req: Request<Body>,
        tx: Sender<trace_utils::SendData>,
    ) -> http::Result<Response<Body>> {
        info!("Recieved traces to process");
        let (parts, body) = req.into_parts();

        if let Some(response) = http_utils::verify_request_content_length(
            &parts.headers,
            config.max_request_content_length,
            "Error processing traces",
        ) {
            return response;
        }

        let tracer_header_tags = (&parts.headers).into();

        // deserialize traces from the request body, convert to protobuf structs (see trace-protobuf
        // crate)
        let (body_size, traces) = match trace_utils::get_traces_from_request_body(body).await {
            Ok(res) => res,
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error deserializing trace from request body: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        let payload = trace_utils::collect_trace_chunks(
            traces,
            &tracer_header_tags,
            |chunk, root_span_index| {
                trace_utils::set_serverless_root_span_tags(
                    &mut chunk.spans[root_span_index],
                    config.function_name.clone(),
                    &config.env_type,
                );
                chunk.spans.retain(|span| {
                    return (span.name != "dns.lookup" && span.resource != "0.0.0.0")
                        || (span.name != "dns.lookup" && span.resource != "127.0.0.1");
                });
                for span in chunk.spans.iter_mut() {
                    debug!("ASTUYVE span is {:?}", span);
                    // trace_utils::enrich_span_with_mini_agent_metadata(span, &mini_agent_metadata);
                    // trace_utils::enrich_span_with_azure_metadata(
                    //     span,
                    //     config.mini_agent_version.as_str(),
                    // );
                    obfuscate_span(span, &config.obfuscation_config);
                }
            },
            true, // In mini agent, we always send agentless
        );

        let send_data = SendData::new(body_size, payload, tracer_header_tags, &config.trace_intake);

        // send trace payload to our trace flusher
        match tx.send(send_data).await {
            Ok(_) => {
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
    use std::{
        collections::HashMap,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::sync::mpsc::{self, Receiver, Sender};

    use crate::traces::{
        config::Config,
        trace_processor::{self, TraceProcessor},
    };
    use datadog_trace_protobuf::pb;
    use datadog_trace_utils::{
        test_utils::{create_test_json_span, create_test_span},
        trace_utils,
    };
    use ddcommon::Endpoint;

    fn get_current_timestamp_nanos() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64
    }

    fn create_test_config() -> Config {
        Config {
            function_name: Some("dummy_function_name".to_string()),
            max_request_content_length: 10 * 1024 * 1024,
            trace_flush_interval: 3,
            stats_flush_interval: 3,
            verify_env_timeout: 100,
            trace_intake: Endpoint {
                url: hyper::Uri::from_static("https://trace.agent.notdog.com/traces"),
                api_key: Some("dummy_api_key".into()),
            },
            trace_stats_intake: Endpoint {
                url: hyper::Uri::from_static("https://trace.agent.notdog.com/stats"),
                api_key: Some("dummy_api_key".into()),
            },
            dd_site: "datadoghq.com".to_string(),
            env_type: trace_utils::EnvironmentType::CloudFunction,
            os: "linux".to_string(),
            obfuscation_config: ObfuscationConfig::new().unwrap(),
            mini_agent_version: "0.1.0".to_string(),
        }
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

        let trace_processor = trace_processor::ServerlessTraceProcessor {};
        let res = trace_processor
            .process_traces(
                request,
                tx,
                Arc::new(trace_utils::MiniAgentMetadata::default()),
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
                priority: i8::MIN as i32,
                origin: "".to_string(),
                spans: vec![create_test_span(11, 222, 333, start, true)],
                tags: HashMap::new(),
                dropped_trace: false,
            }],
            tags: HashMap::new(),
            env: "test-env".to_string(),
            hostname: "".to_string(),
            app_version: "".to_string(),
        };

        assert_eq!(
            expected_tracer_payload,
            tracer_payload.unwrap().get_payloads()[0]
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_process_trace_top_level_span_set() {
        let (tx, mut rx): (
            Sender<trace_utils::SendData>,
            Receiver<trace_utils::SendData>,
        ) = mpsc::channel(1);

        let start = get_current_timestamp_nanos();

        let json_trace = vec![
            create_test_json_span(11, 333, 222, start),
            create_test_json_span(11, 222, 0, start),
            create_test_json_span(11, 444, 333, start),
        ];

        let bytes = rmp_serde::to_vec(&vec![json_trace]).unwrap();
        let request = Request::builder()
            .header("datadog-meta-tracer-version", "4.0.0")
            .header("datadog-meta-lang", "nodejs")
            .header("datadog-meta-lang-version", "v19.7.0")
            .header("datadog-meta-lang-interpreter", "v8")
            .header("datadog-container-id", "33")
            .header("content-length", "100")
            .body(hyper::body::Body::from(bytes))
            .unwrap();

        let trace_processor = trace_processor::ServerlessTraceProcessor {};
        let res = trace_processor
            .process_traces(
                Arc::new(create_test_config()),
                request,
                tx,
                Arc::new(trace_utils::MiniAgentMetadata::default()),
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
                priority: i8::MIN as i32,
                origin: "".to_string(),
                spans: vec![
                    create_test_span(11, 333, 222, start, false),
                    create_test_span(11, 222, 0, start, true),
                    create_test_span(11, 444, 333, start, false),
                ],
                tags: HashMap::new(),
                dropped_trace: false,
            }],
            tags: HashMap::new(),
            env: "test-env".to_string(),
            hostname: "".to_string(),
            app_version: "".to_string(),
        };
        assert_eq!(
            expected_tracer_payload,
            tracer_payload.unwrap().get_payloads()[0]
        );
    }
}
