use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{Sender, self};
use std::thread;

use tracing::debug;

use crate::events::Event;
use crate::logs::{aggregator::Aggregator, datadog, processor::LogsProcessor};
use crate::tags;
use crate::telemetry::events::TelemetryEvent;
use crate::{config, LAMBDA_RUNTIME_SLUG};

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    dd_api: datadog::Api,
    aggregator: Arc<Mutex<Aggregator>>,
    tx: Sender<Vec<TelemetryEvent>>,
    rx: mpsc::Receiver<Vec<TelemetryEvent>>,
    processor: LogsProcessor,
}

impl LogsAgent {
    #[must_use]
    pub fn new(
        tags_provider: Arc<tags::provider::Provider>,
        datadog_config: Arc<config::Config>,
        event_bus: Sender<Event>,
    ) -> LogsAgent {
        let aggregator: Arc<Mutex<Aggregator>> = Arc::new(Mutex::new(Aggregator::default()));
        let processor = LogsProcessor::new(
            Arc::clone(&datadog_config),
            tags_provider,
            event_bus,
            LAMBDA_RUNTIME_SLUG.to_string(),
        );

        let (tx, rx) = mpsc::channel::<Vec<TelemetryEvent>>(1000);

        let dd_api = datadog::Api::new(datadog_config.api_key.clone(), datadog_config.site.clone());
        LogsAgent {
            dd_api,
            aggregator,
            tx,
            rx,
            processor,
        }
    }

    pub async fn spin(&mut self) {
        while let Some(events) = self.rx.recv().await {
            self.processor.process(events, &self.aggregator).await;
        }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<Vec<TelemetryEvent>> {
        self.tx.clone()
    }

    pub async fn flush(&self) {
        LogsAgent::flush_internal(&self.aggregator, &self.dd_api).await;
    }

    async fn flush_internal(aggregator: &Arc<Mutex<Aggregator>>, dd_api: &datadog::Api) {
        let mut guard = aggregator.lock().expect("lock poisoned");
        let logs = guard.get_batch();
        drop(guard);
        dd_api.send(logs).await.expect("Failed to send logs to Datadog");
    }

    async fn flush_shutdown(aggregator: &Arc<Mutex<Aggregator>>, dd_api: &datadog::Api) {
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

    pub async fn shutdown(self) {
        debug!("Shutting down LogsAgent");
        // Dropping this sender to help close the thread
        drop(self.tx);
        LogsAgent::flush_shutdown(&self.aggregator, &self.dd_api).await;
    }
}
