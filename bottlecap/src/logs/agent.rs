use std::sync::Arc;
use tokio::sync::mpsc::{self, Sender};

use crate::event_bus::Event;
use crate::extension::telemetry::events::TelemetryEvent;
use crate::logs::{aggregator_service::AggregatorHandle, processor::LogsProcessor};
use crate::tags;
use crate::{LAMBDA_RUNTIME_SLUG, config};

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
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
        aggregator_handle: AggregatorHandle,
    ) -> Self {
        let processor = LogsProcessor::new(
            Arc::clone(&datadog_config),
            tags_provider,
            event_bus,
            LAMBDA_RUNTIME_SLUG.to_string(),
            aggregator_handle.clone(),
        );

        let (tx, rx) = mpsc::channel::<TelemetryEvent>(1000);

        Self {
            tx,
            rx,
            processor,
        }
    }

    pub async fn spin(&mut self) {
        while let Some(event) = self.rx.recv().await {
            self.processor.process(event).await;
        }
    }

    pub async fn sync_consume(&mut self) {
        if let Some(events) = self.rx.recv().await {
            self.processor.process(events).await;
        }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<TelemetryEvent> {
        self.tx.clone()
    }
}
