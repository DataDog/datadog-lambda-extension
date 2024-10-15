use std::collections::HashMap;

use datadog_trace_protobuf::pb::SpanLink;

#[derive(Copy, Clone, Default, Debug)]
pub struct Sampling {
    pub priority: Option<i8>,
    pub mechanism: Option<u8>,
}

#[derive(Clone, Default, Debug)]
#[allow(clippy::module_name_repetitions)]
pub struct SpanContext {
    pub trace_id: u64,
    pub span_id: u64,
    pub sampling: Option<Sampling>,
    pub origin: Option<String>,
    pub tags: HashMap<String, String>,
    pub links: Vec<SpanLink>,
}
