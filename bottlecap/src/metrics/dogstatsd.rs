use tokio::sync::mpsc::Sender;

use tracing::{error, info};

use crate::config;
use crate::events::{self, Event, MetricEvent};
use crate::metrics::aggregator::Aggregator;
use crate::metrics::constants;
use crate::metrics::datadog;
use crate::metrics::metric::Metric;
use crate::tags::provider;
use std::sync::{Arc, Mutex};

pub struct DogStatsD {
    cancel_token: tokio_util::sync::CancellationToken,
    aggregator: Arc<Mutex<Aggregator<1024>>>,
    socket: tokio::net::UdpSocket,
    event_bus: Sender<events::Event>,
    dd_api: datadog::DdApi,
}

pub struct DogStatsDConfig {
    pub host: String,
    pub port: u16,
    pub datadog_config: Arc<config::Config>,
    pub aggregator: Arc<Mutex<Aggregator<1024>>>,
    pub tags_provider: Arc<provider::Provider>,
}

impl DogStatsD {
    #[must_use]
    pub async fn new(config: &DogStatsDConfig, event_bus: Sender<events::Event>, cancel_token: tokio_util::sync::CancellationToken) -> DogStatsD {
        let dd_api = datadog::DdApi::new(
            config.datadog_config.api_key.clone(),
            config.datadog_config.site.clone(),
        );
        let addr = format!("{}:{}", config.host, config.port);
        // TODO (UDS socket)
        let socket = tokio::net::UdpSocket::bind(addr).await.expect("couldn't bind to address");
        DogStatsD {
            socket,
            cancel_token,
            event_bus: event_bus.clone(),
            aggregator: config.aggregator.clone(),
            dd_api,
        }
    }

    pub async fn spin(self) {
        loop {
            // TODO(astuyve) this should be dynamic
            let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
            let (amt, src) = self.socket.recv_from(&mut buf).await.expect("didn't receive data");
            let buf = &mut buf[..amt];
            let msg = std::str::from_utf8(buf).expect("couldn't parse as string");
            info!(
                "received message: {} from {}, sending it to the bus",
                msg, src
            );
            let parsed_metric = match Metric::parse(msg) {
                Ok(parsed_metric) => {
                    info!("parsed metric: {:?}", parsed_metric);
                    parsed_metric
                }
                Err(e) => {
                    error!("failed to parse metric: {:?}\n message: {:?}", msg, e);
                    continue;
                }
            };
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
            let _ = self.aggregator
                .lock()
                .expect("lock poisoned")
                .insert(&parsed_metric);
            // Don't publish until after validation and adding metric_event to buff
            let _ = self.event_bus.send(Event::Metric(metric_event)); // todo check the result
            if self.cancel_token.is_cancelled() {
                error!("ASTUYVE shutting down DogStatsD server");
                break;
            }
        }
    }

    pub async fn flush(&mut self) {
        let locked_aggr = &mut self.aggregator.lock().expect("lock poisoned");
        let current_points = locked_aggr.to_series();
        let current_distribution_points = locked_aggr.distributions_to_protobuf();
        if !current_points.series.is_empty() {
            let () = &self
                .dd_api
                .ship_series(&current_points)
                .await
                // TODO(astuyve) retry and do not panic
                .expect("failed to ship metrics to datadog");
        }
        if !current_distribution_points.sketches.is_empty() {
            let () = &self
                .dd_api
                .ship_distributions(&current_distribution_points)
                .await
                // TODO(astuyve) retry and do not panic
                .expect("failed to ship metrics to datadog");
        }
        locked_aggr.clear();
    }
}
