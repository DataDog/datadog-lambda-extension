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
        let all_series;
        let all_distributions;
        {
            let locked_aggr = &mut self.aggregator.lock().expect("lock poisoned").clone();
            all_series = locked_aggr.consume_metrics();
            all_distributions = locked_aggr.consume_distributions();
        }
        for a_batch in all_series {
            debug!("flushing {} series to datadog", a_batch.series.len());
            match &self.dd_api.ship_series(&a_batch).await {
                Ok(()) => {}
                Err(e) => {
                    debug!("failed to ship metrics to datadog: {:?}", e);
                }
            }
            // TODO(astuyve) retry and do not panic
        }
        for a_batch in all_distributions {
            match &self.dd_api.ship_distributions(&a_batch).await {
                Ok(()) => {}
                Err(e) => {
                    debug!("failed to ship distributions to datadog: {:?}", e);
                }
            }
        }
    }
}
