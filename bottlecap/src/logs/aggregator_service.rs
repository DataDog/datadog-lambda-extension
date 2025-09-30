use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::logs::aggregator::Aggregator;

#[derive(Debug)]
pub enum AggregatorCommand {
    InsertBatch(Vec<String>),
    Flush(oneshot::Sender<Vec<Vec<u8>>>),
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

    pub async fn flush(&self) -> Result<Vec<Vec<u8>>, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(AggregatorCommand::Flush(response_tx))
            .map_err(|e| format!("Failed to send flush command: {}", e))?;

        response_rx
            .await
            .map_err(|e| format!("Failed to receive flush response: {}", e))
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

    pub fn new_default() -> (Self, AggregatorHandle) {
        use crate::logs::constants;
        Self::new(
            constants::MAX_BATCH_ENTRIES_SIZE,
            constants::MAX_CONTENT_SIZE_BYTES,
            constants::MAX_LOG_SIZE_BYTES,
        )
    }

    pub async fn run(mut self) {
        debug!("Logs aggregator service started");

        while let Some(command) = self.rx.recv().await {
            match command {
                AggregatorCommand::InsertBatch(logs) => {
                    self.aggregator.add_batch(logs);
                }
                AggregatorCommand::Flush(response_tx) => {
                    let mut batches = Vec::new();
                    loop {
                        let batch = self.aggregator.get_batch();
                        if batch.is_empty() {
                            break;
                        }
                        batches.push(batch);
                    }
                    let _ = response_tx.send(batches);
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
        let (mut service, handle) = AggregatorService::new_default();
        
        // Spawn the service
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
        
        // Insert logs using handle
        handle.insert_batch(vec![serialized_log.clone()]).unwrap();
        
        // Flush all batches
        let batches = handle.flush().await.unwrap();
        assert_eq!(batches.len(), 1);
        let serialized_batch = format!("[{}]", serialized_log);
        assert_eq!(batches[0], serialized_batch.as_bytes());
        
        // Shutdown the service
        handle.shutdown().unwrap();
        let _ = service_handle.await;
    }

    #[tokio::test]
    async fn test_aggregator_service_multiple_handles() {
        let (mut service, handle1) = AggregatorService::new_default();
        let handle2 = handle1.clone();
        
        // Spawn the service
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
        
        // Send logs using both handles
        handle1.insert_batch(vec![serialized_log.clone()]).unwrap();
        handle2.insert_batch(vec![serialized_log.clone()]).unwrap();
        
        // Flush all batches
        let batches = handle1.flush().await.unwrap();
        assert_eq!(batches.len(), 2);
        
        let serialized_batch = format!("[{}]", serialized_log);
        assert_eq!(batches[0], serialized_batch.as_bytes());
        assert_eq!(batches[1], serialized_batch.as_bytes());
        
        // Shutdown the service
        handle1.shutdown().unwrap();
        let _ = service_handle.await;
    }

    #[tokio::test]
    async fn test_aggregator_service_empty_flush() {
        let (mut service, handle) = AggregatorService::new_default();
        
        // Spawn the service
        let service_handle = tokio::spawn(async move {
            service.run().await;
        });
        
        // Flush when no logs are inserted
        let batches = handle.flush().await.unwrap();
        assert!(batches.is_empty());
        
        // Shutdown the service
        handle.shutdown().unwrap();
        let _ = service_handle.await;
    }
}
