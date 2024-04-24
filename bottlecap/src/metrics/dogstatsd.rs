use std::sync::mpsc::Sender;

use crate::events::{self, Event, MetricEvent};
use crate::metrics::aggregator::Aggregator;
use crate::metrics::metric::Metric;
use crate::metrics::datadog;
use std::sync::{Arc, Mutex};
use tracing;

pub struct DogStatsD {
    serve_handle: std::thread::JoinHandle<()>,
    aggregator: Arc<Mutex<Aggregator<1024>>>,
    dd_api: datadog::DdApi,
}

pub struct DogStatsDConfig {
    pub host: String,
    pub port: u16,
}
const CONTEXTS: usize = 1024;

impl DogStatsD {
    pub fn run(config: &DogStatsDConfig, event_bus: Sender<events::Event>) -> DogStatsD {
        let aggr: Arc<Mutex<Aggregator<CONTEXTS>>> = Arc::new(Mutex::new(Aggregator::<CONTEXTS>::new().expect("failed to create aggregator")));
        let serializer_aggr = Arc::clone(&aggr);
        let serve_handle = DogStatsD::run_server(&config.host, config.port, event_bus, serializer_aggr);
        let dd_api = datadog::DdApi::new();
        DogStatsD { serve_handle, aggregator: aggr, dd_api }
    }

    fn run_server(host: &str, port: u16, event_bus: Sender<events::Event>, aggregator: Arc<Mutex<Aggregator<CONTEXTS>>>) -> std::thread::JoinHandle<()> {
        let addr = format!("{}:{}", host, port);
        std::thread::spawn(move || {
            let socket = std::net::UdpSocket::bind(addr).expect("couldn't bind to address");
            loop {
                let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
                let (amt, src) = socket.recv_from(&mut buf).expect("didn't receive data");
                let buf = &mut buf[..amt];
                let msg = std::str::from_utf8(buf).expect("couldn't parse as string");
                log::info!(
                    "received message: {} from {}, sending it to the bus",
                    msg,
                    src
                );
                let parsed_metric = match Metric::parse(msg) {
                    Ok(parsed_metric) => {
                        log::info!("parsed metric: {:?}", parsed_metric);
                        parsed_metric
                    }
                    Err(e) => {
                        log::error!("failed to parse metric: {:?}\n message: {:?}", msg, e);
                        continue;
                    }
                };
                //TODO(astuyve): move expect to match, perhaps aggregate multi-value metrics in
                //aggregator or modify metric event to pass multi-value metrics
                let metric_event = MetricEvent::new(parsed_metric.name.to_string(), parsed_metric.first_value().expect("no first metric"), parsed_metric.tags());
                let _ = aggregator.lock().expect("lock poisoned").insert(&parsed_metric);
                log::info!("inserted metric into aggregator");
                // Don't publish until after validation and adding metric_event to buff
                let _ = event_bus.send(Event::Metric(metric_event)); // todo check the result
            }
        })
    }

    pub fn flush(&self) {
        let current_points = &self.aggregator.lock().expect("lock poisoned").to_series();
        if current_points.series.is_empty() {
            return;
        }
        let _ = &self.dd_api.ship(&current_points).expect("failed to ship metrics to datadog");
    }

    pub fn shutdown(self) {
        self.flush();
        match self.serve_handle.join() {
            Ok(_) => {
                log::info!("DogStatsD thread has been shutdown");
            }
            Err(e) => {
                log::error!("Error shutting down the DogStatsD thread: {:?}", e);
            }
        }
    }
}
