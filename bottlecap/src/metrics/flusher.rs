use crate::config::Config;
use crate::metrics::aggregator::Aggregator;
use crate::metrics::datadog;
use std::sync::{Arc, Mutex};

pub struct Flusher {
    dd_api: datadog::DdApi,
    aggregator: Arc<Mutex<Aggregator>>,
}

#[inline]
#[must_use]
pub fn build_fqdn_metrics(site: String) -> String {
    format!("https://api.{site}")
}

#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(
        api_key: String,
        aggregator: Arc<Mutex<Aggregator>>,
        config: Arc<Config>,
        site: String,
    ) -> Self {
        let dd_api = datadog::DdApi::new(api_key, config, site);
        Flusher { dd_api, aggregator }
    }

    pub async fn flush(&mut self) {
        let (all_series, all_distributions) = {
            let mut aggregator = self.aggregator.lock().expect("lock poisoned");
            (
                aggregator.consume_metrics(),
                aggregator.consume_distributions(),
            )
        };
        for a_batch in all_series {
            self.dd_api.ship_series(&a_batch).await;
            // TODO(astuyve) retry and do not panic
        }
        for a_batch in all_distributions {
            self.dd_api.ship_distributions(&a_batch).await;
        }
    }
}
