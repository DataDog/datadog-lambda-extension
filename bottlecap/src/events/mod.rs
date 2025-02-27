use crate::telemetry::events::TelemetryEvent;

#[derive(Debug)]
#[allow(dead_code)] // TODO if this is ever used in practice remove this allow
pub struct MetricEvent {
    name: String,
    value: f64,
    tags: Vec<String>,
}

impl MetricEvent {
    #[must_use]
    pub fn new(name: String, value: f64, tags: Vec<String>) -> MetricEvent {
        MetricEvent { name, value, tags }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct OomEvent {
    pub timestamp: i64,
}

impl OomEvent {
    #[must_use]
    pub fn new(timestamp: i64) -> OomEvent {
        OomEvent { timestamp }
    }
}

#[derive(Debug)]
pub enum Event {
    Metric(MetricEvent),
    Telemetry(TelemetryEvent),
    OutOfMemory(OomEvent),
}
