use libdd_trace_protobuf::pb::Span as DatadogSpan;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use std::{error::Error, sync::Arc};

use crate::{config::Config, otlp::transform::otel_resource_spans_to_dd_spans};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OtlpEncoding {
    Protobuf,
    Json,
}

impl OtlpEncoding {
    #[must_use]
    pub fn from_content_type(content_type: Option<&str>) -> Self {
        match content_type {
            Some(ct) if ct.starts_with("application/json") => OtlpEncoding::Json,
            _ => OtlpEncoding::Protobuf,
        }
    }

    #[must_use]
    pub fn content_type(&self) -> &'static str {
        match self {
            OtlpEncoding::Json => "application/json",
            OtlpEncoding::Protobuf => "application/x-protobuf",
        }
    }
}

#[derive(Clone)]
pub struct Processor {
    config: Arc<Config>,
}

impl Processor {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub fn process(
        &self,
        body: &[u8],
        encoding: OtlpEncoding,
    ) -> Result<Vec<Vec<DatadogSpan>>, Box<dyn Error>> {
        let request = match encoding {
            OtlpEncoding::Json => serde_json::from_slice::<ExportTraceServiceRequest>(body)?,
            OtlpEncoding::Protobuf => ExportTraceServiceRequest::decode(body)?,
        };

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
