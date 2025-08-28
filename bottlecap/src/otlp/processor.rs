use datadog_trace_protobuf::pb::Span as DatadogSpan;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use std::{error::Error, sync::Arc};

use crate::{config::Config, otlp::transform::otel_resource_spans_to_dd_spans};

#[derive(Clone)]
pub struct Processor {
    config: Arc<Config>,
}

impl Processor {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub fn process(&self, body: &[u8], content_type: Option<&str>) -> Result<Vec<Vec<DatadogSpan>>, Box<dyn Error>> {
        // Decode the OTLP HTTP request
        let request: ExportTraceServiceRequest;
        if content_type == Some("application/json") {
            request = serde_json::from_slice::<ExportTraceServiceRequest>(body)?;
        } else {
            request = ExportTraceServiceRequest::decode(body)?;
        }

        let mut spans: Vec<Vec<DatadogSpan>> = Vec::new();
        for resource_spans in &request.resource_spans {
            spans.extend(otel_resource_spans_to_dd_spans(
                resource_spans,
                self.config.clone(),
            ));
        }

        Ok(spans)
    }
}

#[test]
fn test_process() {
    let processor = Processor::new(Arc::new(Config::default()));
    
    let body = r#"
        {"resourceSpans":
            [{"resource":
                {
                    "attributes":[
                        {"key":"service.name","value":{"stringValue":"service-name"}},
                        {"key":"telemetry.sdk.language","value":{"stringValue":"nodejs"}},
                        {"key":"telemetry.sdk.name","value":{"stringValue":"opentelemetry"}},
                        {"key":"telemetry.sdk.version","value":{"stringValue":"1.12.0"}}
                    ],
                    "droppedAttributesCount":0
                },
                "scopeSpans":[
                    {
                        "scope":{"name":"node-otlp-with-extension"},
                        "spans":[
                            {
                                "traceId":"bf38224c14d97588e28dfe92843730e4",
                                "spanId":"f6884f336ff7fe1a",
                                "name":"node-otlp-with-extension",
                                "kind":1,
                                "startTimeUnixNano":1755889353392000000,
                                "endTimeUnixNano":1755889354393503200,
                                "attributes":[],
                                "droppedAttributesCount":0,
                                "events":[],
                                "droppedEventsCount":0,
                                "status":{"code":0},
                                "links":[],
                                "droppedLinksCount":0
                            }
                        ]
                    }
                ]
            }]
        }"#;

    let result = processor.process(body.as_bytes(), Some("application/json"));
    match result {
        Ok(traces) => {
            println!("Processing OTLP trace succeeded with {} trace groups", traces.len());
        }
        Err(e) => {
            println!("Processing OTLP trace failed: {:?}", e);
        }
    }
}