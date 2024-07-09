use crate::metrics::aggregator::Aggregator;
use crate::metrics::datadog;
use crate::metrics::datadog::ShipError;
use std::sync::{Arc, Mutex};
use tracing::debug;

pub struct Flusher {
    dd_api: datadog::DdApi,
    aggregator: Arc<Mutex<Aggregator>>,
}

#[must_use]
pub fn build_fqdn_site(site: String) -> String {
    format!("https://api.{site}")
}

#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(api_key: String, aggregator: Arc<Mutex<Aggregator>>, site: String) -> Self {
        let dd_api = datadog::DdApi::new(api_key, site);
        Flusher { dd_api, aggregator }
    }

    pub async fn flush(&mut self) -> Vec<(String, Option<ShipError>)> {
        let (all_series, all_distributions) = {
            let mut aggregator = self.aggregator.lock().expect("lock poisoned");
            (
                aggregator.consume_metrics(),
                aggregator.consume_distributions(),
            )
        };
        let mut res = Vec::new();
        for (count, a_batch) in all_series.into_iter().enumerate() {
            let req_id = format!("d{count}");
            match self.dd_api.ship_series(&a_batch).await {
                Ok(()) => {
                    res.push((req_id, None));
                }
                Err(e) => {
                    debug!("failed to ship metrics to datadog: {:?}", e);
                    res.push((req_id, Some(e)));
                }
            }
            // TODO(astuyve) retry and do not panic
        }
        for (count, a_batch) in all_distributions.into_iter().enumerate() {
            let req_id = format!("d{count}");
            match self.dd_api.ship_distributions(&a_batch).await {
                Ok(()) => {
                    res.push((req_id, None));
                }
                Err(e) => {
                    debug!("failed to ship distributions to datadog: {:?}", e);
                    res.push((req_id, Some(e)));
                }
            }
        }
        res
    }
}
