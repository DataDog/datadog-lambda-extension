use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error};

use crate::logs::{aggregator::Aggregator, constants};

#[derive(Debug)]
pub enum AggregatorCommand {
    InsertBatch(Vec<String>),
    GetBatches(oneshot::Sender<Vec<Vec<u8>>>),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct AggregatorHandle {
    tx: mpsc::UnboundedSender<AggregatorCommand>,
}

impl AggregatorHandle {
    pub fn insert_batch(
        &self,
        logs: Vec<String>,
    ) -> Result<(), mpsc::error::SendError<AggregatorCommand>> {
        self.tx.send(AggregatorCommand::InsertBatch(logs))
    }

    pub async fn get_batches(&self) -> Result<Vec<Vec<u8>>, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(AggregatorCommand::GetBatches(response_tx))
            .map_err(|e| format!("Failed to send flush command: {e}"))?;

        response_rx
            .await
            .map_err(|e| format!("Failed to receive flush response: {e}"))
    }

    pub fn shutdown(&self) -> Result<(), mpsc::error::SendError<AggregatorCommand>> {
        self.tx.send(AggregatorCommand::Shutdown)
    }
}

pub struct AggregatorService {
    aggregator: Aggregator,
    rx: mpsc::UnboundedReceiver<AggregatorCommand>,
}

impl AggregatorService {
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> (Self, AggregatorHandle) {
        Self::new(
            constants::MAX_BATCH_ENTRIES_SIZE,
            constants::MAX_CONTENT_SIZE_BYTES,
            constants::MAX_LOG_SIZE_BYTES,
        )
    }

    #[must_use]
    pub fn new(
        max_batch_entries_size: usize,
        max_content_size_bytes: usize,
        max_log_size_bytes: usize,
    ) -> (Self, AggregatorHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let aggregator = Aggregator::new(
            max_batch_entries_size,
            max_content_size_bytes,
            max_log_size_bytes,
        );

        let service = Self { aggregator, rx };
        let handle = AggregatorHandle { tx };

        (service, handle)
    }

    pub async fn run(mut self) {
        debug!("Logs aggregator service started");

        while let Some(command) = self.rx.recv().await {
            match command {
                AggregatorCommand::InsertBatch(logs) => {
                    self.aggregator.add_batch(logs);
                }
                AggregatorCommand::GetBatches(response_tx) => {
                    let mut batches = Vec::new();
                    let mut current_batch = self.aggregator.get_batch();
                    while !current_batch.is_empty() {
                        batches.push(current_batch);
                        current_batch = self.aggregator.get_batch();
                    }
                    if response_tx.send(batches).is_err() {
                        error!("Failed to send logs flush response - receiver dropped");
                    }
                }
                AggregatorCommand::Shutdown => {
                    debug!("Logs aggregator service shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::logs::lambda::{IntakeLog, Lambda, Message};

    #[tokio::test]
    async fn test_aggregator_service_insert_and_flush() {
        let (service, handle) = AggregatorService::default();

        let service_handle = tokio::spawn(async move {
            service.run().await;
        });

        let log = IntakeLog {
            message: Message {
                message: "test".to_string(),
                lambda: Lambda {
                    arn: "arn".to_string(),
                    request_id: Some("request_id".to_string()),
                },
                timestamp: 0,
                status: "status".to_string(),
            },
            hostname: "hostname".to_string(),
            service: "service".to_string(),
            tags: "tags".to_string(),
            source: "source".to_string(),
        };
        let serialized_log = serde_json::to_string(&log).unwrap();

        handle.insert_batch(vec![serialized_log.clone()]).unwrap();

        let batches = handle.get_batches().await.unwrap();
        assert_eq!(batches.len(), 1);
        let serialized_batch = format!("[{serialized_log}]");
        assert_eq!(batches[0], serialized_batch.as_bytes());

        handle
            .shutdown()
            .expect("Failed to shutdown aggregator service");
        service_handle
            .await
            .expect("Aggregator service task failed");
    }
}
