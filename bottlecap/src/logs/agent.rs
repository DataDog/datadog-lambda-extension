use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use tracing::{debug, error};

use crate::logs::aggregator::Aggregator;
use crate::logs::datadog::DdApi;
use crate::logs::processor::Processor;
use crate::telemetry::events::TelemetryEvent;

pub struct LogsAgent {
    function_arn: Option<String>,
    dd_api: DdApi,
    aggregator: Arc<Mutex<Aggregator>>,
    tx: Sender<TelemetryEvent>,
    join_handle: std::thread::JoinHandle<()>,
}

impl LogsAgent {
    pub fn run(function_arn: &str) -> LogsAgent {
        let function_arn = function_arn.to_string();
        let aggregator: Arc<Mutex<Aggregator>> = Arc::new(Mutex::new(Aggregator::new()));
        let processor: Processor = Processor::new(function_arn.clone());

        let cloned_aggregator = Arc::clone(&aggregator);

        let (tx, rx) = mpsc::channel::<TelemetryEvent>();
        let join_handle = thread::spawn(move || loop {
            let received = rx.recv();
            if let Ok(event) = received {
                match processor.process(event) {
                    Ok(log) => {
                        cloned_aggregator.lock().expect("lock poisoned").add(log);
                    }
                    Err(e) => {
                        error!("Error processing event: {}", e);
                    }
                }
            }
        });
        LogsAgent {
            tx,
            join_handle,
            dd_api: DdApi::new(),
            aggregator,
            function_arn: None,
        }
    }

    pub fn get_sender_copy(&self) -> Sender<TelemetryEvent> {
        self.tx.clone()
    }

    pub fn set_function_arn(&mut self, function_arn: String) {
        self.function_arn = Some(function_arn);
    }

    pub fn flush(&self) {
        let logs = self.aggregator.lock().expect("lock poisoned").get_batch();
        self.dd_api
            .send(&logs)
            .expect("failed to send logs to Datadog");
    }

    pub fn shutdown(self) {
        debug!("Shutting down LogsAgent");
        self.flush();
        match self.join_handle.join() {
            Ok(_) => {
                debug!("LogsAgent thread has been shutdown");
            }
            Err(e) => {
                debug!("Error shutting down the LogsAgent thread: {:?}", e);
            }
        }
    }
}
