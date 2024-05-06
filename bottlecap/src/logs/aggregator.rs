use crate::logs::processor::IntakeLog;

#[derive(Default)]
pub struct Aggregator {
    messages: Vec<IntakeLog>,
}

impl Aggregator {
    pub fn add(&mut self, log: IntakeLog) {
        self.messages.push(log);
    }

    pub fn get_batch(&mut self) -> Vec<IntakeLog> {
        std::mem::take(&mut self.messages)
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
        assert_eq!(aggregator.messages[0], log);
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
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0], log);
        assert_eq!(aggregator.messages.len(), 0);
    }
}
