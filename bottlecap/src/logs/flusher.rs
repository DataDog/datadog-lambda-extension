use crate::logs::{aggregator::Aggregator, datadog};
use std::sync::{Arc, Mutex};

pub struct Flusher {
    dd_api: datadog::Api,
    aggregator: Arc<Mutex<Aggregator>>,
}

#[allow(clippy::await_holding_lock)]
impl Flusher {
    pub fn new(api_key: Option<String>, aggregator: Arc<Mutex<Aggregator>>, site: String) -> Self {
        let dd_api = datadog::Api::new(api_key, site);
        Flusher { dd_api, aggregator }
    }
    pub async fn flush(&self) {
        let mut guard = self.aggregator.lock().expect("lock poisoned");
        let logs = guard.get_batch();
        drop(guard);
        self.dd_api
            .send(logs)
            .await
            .expect("Failed to send logs to Datadog");
    }

    pub async fn flush_shutdown(aggregator: &Arc<Mutex<Aggregator>>, dd_api: &datadog::Api) {
        let mut aggregator = aggregator.lock().expect("lock poisoned");
        let mut logs = aggregator.get_batch();
        // It could be an empty JSON array: []
        while logs.len() > 2 {
            dd_api
                .send(logs)
                .await
                .expect("Failed to send logs to Datadog");
            logs = aggregator.get_batch();
        }
    }
}
