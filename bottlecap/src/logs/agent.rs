use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use crate::event_bus::Event;
use crate::extension::telemetry::events::TelemetryEvent;
use crate::logs::{aggregator_service::AggregatorHandle, processor::LogsProcessor};
use crate::tags;
use crate::{LAMBDA_RUNTIME_SLUG, config};

const DRAIN_LOG_INTERVAL: Duration = Duration::from_millis(100);

/// `(request_id, execution_id, execution_name)` extracted from an `aws.lambda` span.
pub type DurableContextUpdate = (String, String, String);

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    rx: mpsc::Receiver<TelemetryEvent>,
    durable_context_rx: mpsc::Receiver<DurableContextUpdate>,
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
    ) -> (Self, Sender<TelemetryEvent>, Sender<DurableContextUpdate>) {
        let processor = LogsProcessor::new(
            Arc::clone(&datadog_config),
            tags_provider,
            event_bus,
            LAMBDA_RUNTIME_SLUG.to_string(),
            is_managed_instance_mode,
        );

        let (tx, rx) = mpsc::channel::<TelemetryEvent>(1000);
        let (durable_context_tx, durable_context_rx) = mpsc::channel::<DurableContextUpdate>(500);
        let cancel_token = CancellationToken::new();

        let agent = Self {
            rx,
            durable_context_rx,
            processor,
            aggregator_handle,
            cancel_token,
        };

        (agent, tx, durable_context_tx)
    }

    pub async fn spin(&mut self) {
        loop {
            tokio::select! {
                Some(event) = self.rx.recv() => {
                    self.processor.process(event, &self.aggregator_handle).await;
                }
                Some((request_id, execution_id, execution_name)) = self.durable_context_rx.recv() => {
                    self.processor.insert_to_durable_map(&request_id, &execution_id, &execution_name);
                    let ready_logs = self.processor.take_ready_logs();
                    if !ready_logs.is_empty() && let Err(e) = self.aggregator_handle.insert_batch(ready_logs) {
                        error!("LOGS_AGENT | Failed to insert batch: {}", e);
                    }
                }
                () = self.cancel_token.cancelled() => {
                    debug!("LOGS_AGENT | Received shutdown signal, draining remaining events");

                    // Drain remaining events
                    let mut last_drain_log_time = Instant::now().checked_sub(DRAIN_LOG_INTERVAL).expect("Failed to subtract interval from now");
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
                                // Log at most once every 100ms to avoid spamming the logs
                                let now = Instant::now();
                                if now.duration_since(last_drain_log_time) >= DRAIN_LOG_INTERVAL {
                                    debug!("LOGS_AGENT | No more events to process but still have senders, continuing to drain...");
                                    last_drain_log_time = now;
                                }
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
