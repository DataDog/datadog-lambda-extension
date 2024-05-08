use std::collections::VecDeque;
use tracing::{debug, warn};

use crate::logs::constants;
use crate::logs::processor::IntakeLog;
#[derive(Default)]
pub struct Aggregator {
    messages: VecDeque<String>,
}

impl Aggregator {
    pub fn add(&mut self, log: IntakeLog) {
        match serde_json::to_string(&log) {
            Ok(log) => self.messages.push_back(log),
            Err(e) => debug!("Failed to serialize log: {}", e),
        }
    }

    pub fn get_batch(&mut self) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::with_capacity(constants::MAX_CONTENT_SIZE_BYTES);
        buffer.extend(b"[");

        // Fill the batch with logs from the messages
        for index in 0..constants::MAX_BATCH_ENTRIES_SIZE {
            if index > 0 {
                buffer.extend(b",");
            }

            if let Some(log) = self.messages.pop_front() {
                // Check if the buffer will be full after adding the log
                if buffer.len() + log.len() > constants::MAX_CONTENT_SIZE_BYTES {
                    // Put the log back in the queue
                    self.messages.push_front(log);
                    break;
                }

                if log.len() > constants::MAX_LOG_SIZE_BYTES {
                    warn!(
                        "Log size exceeds the 1MB limit: {}, will be truncated by the backend.",
                        log.len()
                    );
                }

                buffer.extend(log.as_bytes());
            } else {
                break;
            }
        }
        // Make sure we added at least one element
        if buffer.len() > 1 {
            // Remove the last comma
            buffer.pop();
        }

        buffer.extend(b"]");

        buffer
    }
}

#[cfg(test)]
mod tests {
    use crate::logs::processor::{Lambda, LambdaMessage};

    use super::*;

    #[test]
    fn add() {
        let mut aggregator = Aggregator::default();
        let log = IntakeLog {
            message: LambdaMessage {
                message: "test".to_string(),
                lambda: Lambda {
                    arn: "arn".to_string(),
                    request_id: "request_id".to_string(),
                },
                timestamp: 0,
                status: "status".to_string(),
            },
            hostname: "hostname".to_string(),
            service: "service".to_string(),
            tags: "tags".to_string(),
            source: "source".to_string(),
        };
        aggregator.add(log.clone());
        assert_eq!(aggregator.messages.len(), 1);
        assert_eq!(aggregator.messages[0], serde_json::to_string(&log).unwrap());
    }

    #[test]
    fn get_batch() {
        let mut aggregator = Aggregator::default();
        let log = IntakeLog {
            message: LambdaMessage {
                message: "test".to_string(),
                lambda: Lambda {
                    arn: "arn".to_string(),
                    request_id: "request_id".to_string(),
                },
                timestamp: 0,
                status: "status".to_string(),
            },
            hostname: "hostname".to_string(),
            service: "service".to_string(),
            tags: "tags".to_string(),
            source: "source".to_string(),
        };
        aggregator.add(log.clone());
        assert_eq!(aggregator.messages.len(), 1);
        let batch = aggregator.get_batch();
        let serialized_batch = format!("[{}]", serde_json::to_string(&log).unwrap());
        assert_eq!(batch, serialized_batch.as_bytes());
    }
}
