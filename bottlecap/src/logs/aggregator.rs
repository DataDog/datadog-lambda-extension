use crate::logs::processor::IntakeLog;

pub struct Aggregator {
    messages: Vec<IntakeLog>,
}

impl Aggregator {
    pub fn new() -> Self {
        Aggregator {
            messages: Vec::new(),
        }
    }

    pub fn add(&mut self, log: IntakeLog) {
        self.messages.push(log);
    }

    pub fn get_batch(&mut self) -> Vec<IntakeLog> {
        let messages = self.messages.clone();
        self.messages.clear();
        messages
    }
}
