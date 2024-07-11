use tokio::sync::mpsc::Sender;

use tracing::{debug, error};

use crate::events::{self, Event, MetricEvent};
use crate::metrics::aggregator::Aggregator;
use crate::metrics::metric::Metric;
use std::sync::{Arc, Mutex};

pub struct DogStatsD {
    cancel_token: tokio_util::sync::CancellationToken,
    aggregator: Arc<Mutex<Aggregator>>,
    socket: tokio::net::UdpSocket,
    event_bus: Sender<events::Event>,
}

pub struct DogStatsDConfig {
    pub host: String,
    pub port: u16,
}

impl DogStatsD {
    #[must_use]
    pub async fn new(
        config: &DogStatsDConfig,
        aggregator: Arc<Mutex<Aggregator>>,
        event_bus: Sender<events::Event>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> DogStatsD {
        let addr = format!("{}:{}", config.host, config.port);
        // TODO (UDS socket)
        let socket = tokio::net::UdpSocket::bind(addr)
            .await
            .expect("couldn't bind to address");
        DogStatsD {
            cancel_token,
            aggregator,
            socket,
            event_bus,
        }
    }

    pub async fn spin(self) {
        loop {
            // TODO(astuyve) this should be dynamic
            let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
            let (amt, src) = self
                .socket
                .recv_from(&mut buf)
                .await
                .expect("didn't receive data");
            let buf = &mut buf[..amt];
            let msg = std::str::from_utf8(buf).expect("couldn't parse as string");
            debug!(
                "received message: {} from {}, sending it to the bus",
                msg, src
            );
            let parsed_metric = match Metric::parse(msg) {
                Ok(parsed_metric) => {
                    debug!("parsed metric: {:?}", parsed_metric);
                    parsed_metric
                }
                Err(e) => {
                    error!("failed to parse metric: {:?}\n message: {:?}", msg, e);
                    continue;
                }
            };
            if parsed_metric.name == "aws.lambda.enhanced.invocations" {
                debug!("dropping invocation metric from layer, as it's set by agent");
                continue;
            }
            let first_value = match parsed_metric.first_value() {
                Ok(val) => val,
                Err(e) => {
                    error!("failed to parse metric: {:?}\n message: {:?}", msg, e);
                    continue;
                }
            };
            let metric_event = MetricEvent::new(
                parsed_metric.name.to_string(),
                first_value,
                parsed_metric.tags(),
            );
            let _ = self
                .aggregator
                .lock()
                .expect("lock poisoned")
                .insert(&parsed_metric);
            // Don't publish until after validation and adding metric_event to buff
            let _ = self.event_bus.send(Event::Metric(metric_event)).await; // todo check the result
            if self.cancel_token.is_cancelled() {
                debug!("closing dogstatsd listener");
                break;
            }
        }
    }
}
