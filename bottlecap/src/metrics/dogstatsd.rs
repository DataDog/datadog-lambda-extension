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
    serve_handle: tokio::task::JoinHandle<()>,
    aggregator: Arc<Mutex<Aggregator<1024>>>,
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
    pub async fn run(config: &DogStatsDConfig, event_bus: Sender<events::Event>) -> DogStatsD {
        let serializer_aggr = Arc::clone(&config.aggregator);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let serve_handle = DogStatsD::run_server(
            &config.host,
            config.port,
            event_bus,
            serializer_aggr,
            cancel_token.clone(),
        )
        .await;
        let dd_api = datadog::DdApi::new(
            config.datadog_config.api_key.clone(),
            config.datadog_config.site.clone(),
        );
        DogStatsD {
            serve_handle,
            cancel_token,
            aggregator: config.aggregator.clone(),
            dd_api,
        }
    }

    async fn run_server(
        host: &str,
        port: u16,
        event_bus: Sender<events::Event>,
        aggregator: Arc<Mutex<Aggregator<{ constants::CONTEXTS }>>>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let addr = format!("{host}:{port}");
        tokio::spawn(async move {
            let socket = std::net::UdpSocket::bind(addr).expect("couldn't bind to address");
            loop {
                // TODO(astuyve) this should be dynamic
                let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
                let (amt, src) = socket.recv_from(&mut buf).expect("didn't receive data");
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
                let _ = aggregator
                    .lock()
                    .expect("lock poisoned")
                    .insert(&parsed_metric);
                // Don't publish until after validation and adding metric_event to buff
                let _ = event_bus.send(Event::Metric(metric_event)); // todo check the result
                if cancel_token.is_cancelled() {
                    error!("ASTUYVE shutting down DogStatsD server");
                    break;
                }
            }
        })
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

    pub async fn shutdown(mut self) {
        self.cancel_token.cancel();
        self.flush().await;
        match self.serve_handle.await {
            Ok(()) => {
                info!("DogStatsD thread has been shutdown");
            }
            Err(e) => {
                error!("Error shutting down the DogStatsD thread: {:?}", e);
            }
        }
    }
}
