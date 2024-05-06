use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use tracing::{debug, error};

use crate::config;
use crate::logs::aggregator::Aggregator;
use crate::logs::datadog;
use crate::logs::processor::Processor;
use crate::telemetry::events::TelemetryEvent;

pub struct LogsAgent {
    dd_api: datadog::DdApi,
    aggregator: Arc<Mutex<Aggregator>>,
    tx: Sender<TelemetryEvent>,
    join_handle: std::thread::JoinHandle<()>,
}

impl LogsAgent {
    pub fn run(function_arn: &str, datadog_config: Arc<config::Config>) -> LogsAgent {
        let function_arn = function_arn.to_string();
        let aggregator: Arc<Mutex<Aggregator>> = Arc::new(Mutex::new(Aggregator::default()));
        let processor: Processor = Processor::new(function_arn, Arc::clone(&datadog_config));

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

        let dd_api =
            datadog::DdApi::new(datadog_config.api_key.clone(), datadog_config.site.clone());
        LogsAgent {
            tx,
            join_handle,
            dd_api,
            aggregator,
        }
    }

    pub fn get_sender_copy(&self) -> Sender<TelemetryEvent> {
        self.tx.clone()
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
