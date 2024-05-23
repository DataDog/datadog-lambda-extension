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

#[derive(Debug)]
pub enum Event {
    Metric(MetricEvent),
    Telemetry(TelemetryEvent),
    LogsBatch(Vec<TelemetryEvent>),
}
