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
    dd_api: datadog::Api,
    aggregator: Arc<Mutex<Aggregator>>,
    tx: Sender<TelemetryEvent>,
    join_handle: std::thread::JoinHandle<()>,
}

impl LogsAgent {
    pub fn run(function_arn: &str, datadog_config: Arc<config::Config>) -> LogsAgent {
        let function_arn = function_arn.to_string();
        let aggregator: Arc<Mutex<Aggregator>> = Arc::new(Mutex::new(Aggregator::default()));
        let processor: Processor = Processor::new(function_arn, Arc::clone(&datadog_config));

        let cloned_aggregator = aggregator.clone();

        let (tx, rx) = mpsc::channel::<TelemetryEvent>();
        let join_handle = thread::spawn(move || loop {
            let received = rx.recv();
            // TODO(duncanista): we might need to create a Event::Shutdown
            // to signal shutdown and make it easier to handle any floating events
            let Ok(event) = received else {
                debug!("Failed to received event in Logs Agent");
                break;
            };

            match processor.process(event) {
                Ok(log) => {
                    cloned_aggregator.lock().expect("lock poisoned").add(log);
                }
                Err(e) => {
                    error!("Error processing event: {}", e);
                }
            }
        });

        let dd_api = datadog::Api::new(datadog_config.api_key.clone(), datadog_config.site.clone());
        LogsAgent {
            tx,
            join_handle,
            dd_api,
            aggregator,
        }
    }

    pub fn send_event(&self, event: TelemetryEvent) {
        if let Err(e) = self.tx.send(event) {
            error!("Error sending Telemetry event to the Logs Agent: {}", e);
        }
    }

    pub fn flush(&self) {
        LogsAgent::flush_internal(&self.aggregator, &self.dd_api);
    }

    fn flush_internal(aggregator: &Arc<Mutex<Aggregator>>, dd_api: &datadog::Api) {
        let logs = aggregator.lock().expect("lock poisoned").get_batch();
        dd_api.send(&logs).expect("Failed to send logs to Datadog");
    }

    pub fn shutdown(self) {
        debug!("Shutting down LogsAgent");
        // Dropping this sender to help close the thread
        drop(self.tx);
        match self.join_handle.join() {
            Ok(_) => {
                debug!("LogsAgent Message Receiver thread has been shutdown");
            }
            Err(e) => {
                debug!(
                    "Error shutting down the LogsAgent Message Receiver thread: {:?}",
                    e
                );
            }
        }
        LogsAgent::flush_internal(&self.aggregator, &self.dd_api);
    }
}
