use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{self, Sender};

use crate::event_bus::Event;
use crate::logs::{aggregator::Aggregator, processor::LogsProcessor};
use crate::tags;
use crate::telemetry::events::TelemetryEvent;
use crate::{LAMBDA_RUNTIME_SLUG, config};

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    pub aggregator: Arc<Mutex<Aggregator>>,
    tx: Sender<TelemetryEvent>,
    rx: mpsc::Receiver<TelemetryEvent>,
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

        let (tx, rx) = mpsc::channel::<TelemetryEvent>(1000);

        LogsAgent {
            aggregator,
            tx,
            rx,
            processor,
        }
    }

    pub async fn spin(&mut self) {
        while let Some(event) = self.rx.recv().await {
            self.processor.process(event, &self.aggregator).await;
        }
    }

    pub async fn sync_consume(&mut self) {
        if let Some(events) = self.rx.recv().await {
            self.processor.process(events, &self.aggregator).await;
        }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<TelemetryEvent> {
        self.tx.clone()
    }
}
