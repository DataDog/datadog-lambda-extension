#[derive(Debug)]

pub struct MetricEvent {
    name: String,
    value: f64,
    tags: Vec<String>,
}

impl MetricEvent {
    pub fn new(name: String, value: f64, tags: Vec<String>) -> MetricEvent {
        MetricEvent { name, value, tags }
    }
}

#[derive(Debug)]
pub enum Event {
    Metric(MetricEvent),
    Unknown,
}
