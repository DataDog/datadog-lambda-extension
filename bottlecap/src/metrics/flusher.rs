use crate::metrics::aggregator::Aggregator;
use crate::metrics::datadog;
use std::sync::{Arc, Mutex};
use tracing::debug;

pub struct Flusher {
    dd_api: datadog::DdApi,
    aggregator: Arc<Mutex<Aggregator<1024>>>,
}

#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(api_key: String, aggregator: Arc<Mutex<Aggregator<1024>>>, site: String) -> Self {
        let dd_api = datadog::DdApi::new(api_key, site);
        Flusher { dd_api, aggregator }
    }

    pub async fn flush(&mut self) {
        let locked_aggr = &mut self.aggregator.lock().expect("lock poisoned");
        let mut current_points = locked_aggr.to_series_serialized();
        while !current_points.is_empty() {
            debug!("flushing {} series to datadog", current_points.len());
            match &self.dd_api.ship_series(current_points).await {
                Ok(()) => {}
                Err(e) => {
                    debug!("failed to ship metrics to datadog: {:?}", e);
                }
            }
            current_points = locked_aggr.to_series_serialized();
            // TODO(astuyve) retry and do not panic
        }
        let mut current_distribution_points = locked_aggr.distributions_to_protobuf_serialized();
        while !current_distribution_points.is_empty() {
            match &self
                .dd_api
                .ship_distributions(current_distribution_points)
                .await
            {
                Ok(()) => {}
                Err(e) => {
                    debug!("failed to ship distributions to datadog: {:?}", e);
                }
            }
            current_distribution_points = locked_aggr.distributions_to_protobuf_serialized();
        }
        locked_aggr.clear();
    }
}
