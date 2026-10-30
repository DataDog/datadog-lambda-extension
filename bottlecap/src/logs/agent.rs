use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, Sender};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::event_bus::Event;
use crate::extension::telemetry::events::TelemetryEvent;
use crate::logs::{aggregator_service::AggregatorHandle, processor::LogsProcessor};
use crate::tags;
use crate::{LAMBDA_RUNTIME_SLUG, config};

/// Upper bound on waiting for the Telemetry API listener to close the channel at shutdown.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

use crate::logs::lambda::DurableContextUpdate;

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    rx: mpsc::Receiver<TelemetryEvent>,
    durable_context_rx: mpsc::Receiver<DurableContextUpdate>,
    processor: LogsProcessor,
    aggregator_handle: AggregatorHandle,
    event_bus: Sender<Event>,
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
            event_bus.clone(),
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
            event_bus,
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
                Some(update) = self.durable_context_rx.recv() => {
                    self.processor.process_durable_context_update(update, &self.aggregator_handle);
                }
                () = self.cancel_token.cancelled() => {
                    debug!("LOGS_AGENT | Received shutdown signal, draining remaining events");

                    // Drain until the listener has shut down and dropped its sender, so every
                    // telemetry event reaches the event bus ahead of the Tombstone.
                    let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
                    loop {
                        match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                            Ok(Some(event)) => {
                                self.processor.process(event, &self.aggregator_handle).await;
                            }
                            Ok(None) => {
                                debug!("LOGS_AGENT | Channel disconnected, finished draining");
                                break;
                            }
                            Err(_) => {
                                debug!("LOGS_AGENT | Timed out waiting for channel to disconnect");
                                break;
                            }
                        }
                    }

                    // Drain any pending durable context updates before flushing held logs,
                    // to maximise the chance of decorating logs with execution context.
                    while let Ok(update) = self.durable_context_rx.try_recv() {
                        self.processor.process_durable_context_update(update, &self.aggregator_handle);
                    }

                    // Drain remaining held logs without durable context tags so no logs are lost.
                    self.processor.drain_held_logs(&self.aggregator_handle);

                    if let Err(e) = self.event_bus.send(Event::Tombstone).await {
                        debug!("LOGS_AGENT | Failed to send tombstone event: {e}");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_bus::EventBus;
    use crate::extension::telemetry::events::TelemetryRecord;
    use crate::logs::aggregator_service::AggregatorService;
    use std::collections::HashMap;

    const RUNTIME_DONE: &str = r#"{"time":"2022-10-21T14:05:05.764Z","type":"platform.runtimeDone","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","status":"timeout","metrics":{"durationMs":3000.0,"producedBytes":0}}}"#;
    const FUNCTION_LOG: &str =
        r#"{"time":"2022-10-21T14:05:04.000Z","type":"function","record":"hello"}"#;

    fn agent() -> (LogsAgent, Sender<TelemetryEvent>, EventBus) {
        let config = Arc::new(config::Config::default());
        let tags_provider = Arc::new(tags::provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));
        let (event_bus, event_bus_tx) = EventBus::run();
        let (aggregator_service, aggregator_handle) = AggregatorService::default();
        tokio::spawn(aggregator_service.run());
        let (agent, logs_tx, _) = LogsAgent::new(
            tags_provider,
            config,
            event_bus_tx,
            aggregator_handle,
            false,
        );
        (agent, logs_tx, event_bus)
    }

    #[tokio::test]
    async fn test_tombstone_follows_events_sent_after_cancel() {
        let (mut agent, logs_tx, mut event_bus) = agent();
        let cancel_token = agent.cancel_token();
        let handle = tokio::spawn(async move { agent.spin().await });

        // A telemetry POST that is still in flight when shutdown starts.
        cancel_token.cancel();
        tokio::task::yield_now().await;
        logs_tx
            .send(serde_json::from_str(RUNTIME_DONE).expect("valid event"))
            .await
            .expect("agent is draining");
        drop(logs_tx);

        let first = event_bus.rx.recv().await.expect("event");
        assert!(matches!(
            first,
            Event::Telemetry(TelemetryEvent {
                record: TelemetryRecord::PlatformRuntimeDone { .. },
                ..
            })
        ));
        assert!(matches!(event_bus.rx.recv().await, Some(Event::Tombstone)));
        handle.await.expect("agent task");
    }

    #[tokio::test]
    async fn test_tombstone_sent_when_sender_never_closes() {
        let (mut agent, _logs_tx, mut event_bus) = agent();
        agent.cancel_token().cancel();
        agent.spin().await;

        assert!(matches!(event_bus.rx.try_recv(), Ok(Event::Tombstone)));
    }

    #[tokio::test]
    async fn test_queued_events_forwarded_after_drain_timeout() {
        const FILL: usize = 150;
        let (mut agent, logs_tx, mut event_bus) = agent();
        let runtime_done: TelemetryEvent = serde_json::from_str(RUNTIME_DONE).expect("valid event");
        let function_log: TelemetryEvent = serde_json::from_str(FUNCTION_LOG).expect("valid event");
        // Events still queued when the drain deadline passes must be forwarded before the
        // Tombstone: `timeout_at` polls `recv` first, and `recv` is ready while events are
        // queued. The run of function logs, which aren't forwarded, makes the agent drain past
        // the deadline without yielding.
        for _ in 0..FILL {
            logs_tx
                .send(runtime_done.clone())
                .await
                .expect("channel open");
        }
        for _ in 0..500 {
            logs_tx
                .send(function_log.clone())
                .await
                .expect("channel open");
        }
        logs_tx.send(runtime_done).await.expect("channel open");
        agent.cancel_token().cancel();
        let handle = tokio::spawn(async move { agent.spin().await });

        // Leave the event bus full until the drain deadline has passed.
        tokio::time::sleep(DRAIN_TIMEOUT + Duration::from_millis(50)).await;

        let mut forwarded = 0;
        loop {
            match event_bus.rx.recv().await.expect("event") {
                Event::Telemetry(_) => forwarded += 1,
                Event::Tombstone => break,
                Event::OutOfMemory { .. } => {}
            }
        }
        assert_eq!(forwarded, FILL + 1);
        handle.await.expect("agent task");
    }
}
