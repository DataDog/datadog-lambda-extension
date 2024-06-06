use crate::metrics::datadog;
use crate::{config, metrics::aggregator::Aggregator};
use std::sync::{Arc, Mutex};
use tracing::debug;

pub struct Flusher {
    dd_api: datadog::DdApi,
    aggregator: Arc<Mutex<Aggregator<1024>>>,
}

impl Flusher {
    pub fn new(config: Arc<config::Config>, aggregator: Arc<Mutex<Aggregator<1024>>>) -> Self {
        let dd_api = datadog::DdApi::new(config.api_key.clone(), config.site.clone());
        Flusher { dd_api, aggregator }
    }

    pub async fn flush(&mut self) {
        let locked_aggr = &mut self.aggregator.lock().expect("lock poisoned");
        let current_points = locked_aggr.to_series();
        let current_distribution_points = locked_aggr.distributions_to_protobuf();
        if !current_points.series.is_empty() {
            debug!("flushing {} series to datadog", current_points.series.len());
            match &self.dd_api.ship_series(&current_points).await {
                Ok(()) => {}
                Err(e) => {
                    debug!("failed to ship metrics to datadog: {:?}", e);
                }
            }
            // TODO(astuyve) retry and do not panic
        }
        if !current_distribution_points.sketches.is_empty() {
            match &self
                .dd_api
                .ship_distributions(&current_distribution_points)
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    debug!("failed to ship distributions to datadog: {:?}", e);
                }
            }
        }
        locked_aggr.clear();
    }
}
