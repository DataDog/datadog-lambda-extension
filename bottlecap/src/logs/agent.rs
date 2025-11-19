use std::sync::Arc;
use tokio::sync::mpsc::{self, Sender};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::event_bus::Event;
use crate::extension::telemetry::events::TelemetryEvent;
use crate::logs::{aggregator_service::AggregatorHandle, processor::LogsProcessor};
use crate::tags;
use crate::{LAMBDA_RUNTIME_SLUG, config};

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    rx: mpsc::Receiver<TelemetryEvent>,
    processor: LogsProcessor,
    aggregator_handle: AggregatorHandle,
    cancel_token: CancellationToken,
}

impl LogsAgent {
    #[must_use]
    pub fn new(
        tags_provider: Arc<tags::provider::Provider>,
        datadog_config: Arc<config::Config>,
        event_bus: Sender<Event>,
        aggregator_handle: AggregatorHandle,
        is_managed_instance_mode: bool,
    ) -> (Self, Sender<TelemetryEvent>) {
        let processor = LogsProcessor::new(
            Arc::clone(&datadog_config),
            tags_provider,
            event_bus,
            LAMBDA_RUNTIME_SLUG.to_string(),
            is_managed_instance_mode,
        );

        let (tx, rx) = mpsc::channel::<TelemetryEvent>(1000);
        let cancel_token = CancellationToken::new();

        let agent = Self {
            rx,
            processor,
            aggregator_handle,
            cancel_token,
        };

        (agent, tx)
    }

    pub async fn spin(&mut self) {
        loop {
            tokio::select! {
                Some(event) = self.rx.recv() => {
                    self.processor.process(event, &self.aggregator_handle).await;
                }
                () = self.cancel_token.cancelled() => {
                    debug!("LOGS_AGENT | Received shutdown signal, draining remaining events");

                    // Drain remaining events
                    'drain_logs_loop: loop {
                        match self.rx.try_recv() {
                            Ok(event) => {
                                self.processor.process(event, &self.aggregator_handle).await;
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                                debug!("LOGS_AGENT | Channel disconnected, finished draining");
                                break 'drain_logs_loop;
                            },
                            // Empty signals there are still outstanding senders
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                                debug!("LOGS_AGENT | No more events to process but still have senders, continuing to drain...");
                            },
                        }
                    }

                    break;
                }
            }
        }
    }

    pub async fn sync_consume(&mut self) {
        tokio::select! {
            Some(events) = self.rx.recv() => {
                self.processor
                    .process(events, &self.aggregator_handle)
                    .await;
            }
            () = self.cancel_token.cancelled() => {
                // Cancellation requested, exit early
            }
        }
    }

    #[must_use]
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.clone()
    }
}
