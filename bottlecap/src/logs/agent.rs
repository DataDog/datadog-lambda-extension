use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, Sender};

use crate::events::Event;
use crate::logs::{aggregator::Aggregator, processor::LogsProcessor};
use crate::tags;
use crate::telemetry::events::TelemetryEvent;
use crate::{config, LAMBDA_RUNTIME_SLUG};

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    pub aggregator: Arc<Mutex<Aggregator>>,
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

        LogsAgent {
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
}
