use std::collections::VecDeque;
use tracing::warn;

use crate::logs::constants;

pub struct Aggregator {
    messages: VecDeque<String>,
    max_batch_entries_size: usize,
    max_content_size_bytes: usize,
    max_log_size_bytes: usize,
}

impl Default for Aggregator {
    fn default() -> Self {
        Aggregator {
            messages: VecDeque::new(),
            max_batch_entries_size: constants::MAX_BATCH_ENTRIES_SIZE,
            max_content_size_bytes: constants::MAX_CONTENT_SIZE_BYTES,
            max_log_size_bytes: constants::MAX_LOG_SIZE_BYTES,
        }
    }
}

impl Aggregator {
    #[allow(dead_code)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(
        max_batch_entries_size: usize,
        max_content_size_bytes: usize,
        max_log_size_bytes: usize,
    ) -> Self {
        Aggregator {
            messages: VecDeque::new(),
            max_batch_entries_size,
            max_content_size_bytes,
            max_log_size_bytes,
        }
    }

    pub fn add_batch(&mut self, logs: Vec<String>) {
        for log in logs {
            println!("log in aggr: {}", log);
            self.messages.push_back(log);
        }
    }

    pub fn get_batch(&mut self) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::with_capacity(self.max_content_size_bytes);
        buffer.extend(b"[");

        // Fill the batch with logs from the messages
        for _ in 0..self.max_batch_entries_size {
            if let Some(log) = self.messages.pop_front() {
                // Check if the buffer will be full after adding the log
                if buffer.len() + log.len() > self.max_content_size_bytes {
                    // Put the log back in the queue
                    self.messages.push_front(log);
                    break;
                }

                if log.len() > self.max_log_size_bytes {
                    warn!(
                        "Log size exceeds the 1MB limit: {}, will be truncated by the backend.",
                        log.len()
                    );
                }

                buffer.extend(log.as_bytes());
                buffer.extend(b",");
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
    use super::*;

    use crate::logs::lambda::{IntakeLog, Lambda, Message};

    #[test]
    fn test_add_batch() {
        let mut aggregator = Aggregator::default();
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
        aggregator.add_batch(vec![serialized_log.clone()]);
        assert_eq!(aggregator.messages.len(), 1);
        assert_eq!(aggregator.messages[0], serialized_log);
    }

    #[test]
    fn test_get_batch() {
        let mut aggregator = Aggregator::default();
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
        aggregator.add_batch(vec![serialized_log.clone()]);
        assert_eq!(aggregator.messages.len(), 1);
        let batch = aggregator.get_batch();
        let serialized_batch = format!("[{}]", serde_json::to_string(&log).unwrap());
        assert_eq!(batch, serialized_batch.as_bytes());
    }

    #[test]
    fn test_get_batch_full_entries() {
        let mut aggregator = Aggregator::new(2, 1_024, 1_024);
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
        // Add 3 logs
        let serialized_log = serde_json::to_string(&log).unwrap();
        aggregator.add_batch(vec![
            serialized_log.clone(),
            serialized_log.clone(),
            serialized_log.clone(),
        ]);

        // The batch should only contain the first 2 logs
        let first_batch = aggregator.get_batch();
        let serialized_log = serde_json::to_string(&log).unwrap();
        let serialized_batch = format!("[{serialized_log},{serialized_log}]");
        assert_eq!(first_batch, serialized_batch.as_bytes());
        assert_eq!(aggregator.messages.len(), 1);

        // The second batch should only contain the last log
        let second_batch = aggregator.get_batch();
        let serialized_batch = format!("[{serialized_log}]");
        assert_eq!(second_batch, serialized_batch.as_bytes());
        assert_eq!(aggregator.messages.len(), 0);
    }

    #[test]
    fn test_get_batch_full_payload() {
        let mut aggregator = Aggregator::new(2, 256, 1_024);
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
        // Add 2 logs
        let serialized_log = serde_json::to_string(&log).unwrap();
        aggregator.add_batch(vec![serialized_log.clone()]);

        // This log will exceed the max content size
        let mut big_log = log.clone();
        big_log.message.message = "a".repeat(256);
        let serialized_big_log = serde_json::to_string(&log).unwrap();
        aggregator.add_batch(vec![serialized_big_log.clone()]);

        let first_batch = aggregator.get_batch();
        let serialized_log = serde_json::to_string(&log).unwrap();
        let serialized_batch = format!("[{serialized_log}]");
        assert_eq!(first_batch, serialized_batch.as_bytes());

        // I really doubt someone would make a log that is 5MB long,
        // so we never send it, but we still keep it in the queue.
        assert_eq!(aggregator.messages.len(), 1);
    }
}
