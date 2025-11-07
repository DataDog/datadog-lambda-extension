use libdd_trace_protobuf::pb::Span as DatadogSpan;
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

    pub fn process(&self, body: &[u8]) -> Result<Vec<Vec<DatadogSpan>>, Box<dyn Error>> {
        // Decode the OTLP HTTP request
        let request = ExportTraceServiceRequest::decode(body)?;

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
