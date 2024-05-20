use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use tracing::{debug, error};

use crate::logs::aggregator::Aggregator;
use crate::logs::datadog;
use crate::logs::processor;
use crate::tags;
use crate::telemetry::events::TelemetryEvent;
use crate::{config, LAMBDA_RUNTIME_SLUG};

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    dd_api: datadog::Api,
    aggregator: Arc<Mutex<Aggregator>>,
    tx: Sender<TelemetryEvent>,
    join_handle: std::thread::JoinHandle<()>,
}

impl LogsAgent {
    #[must_use]
    pub fn run(
        tags_provider: Arc<tags::provider::Provider>,
        datadog_config: Arc<config::Config>,
    ) -> LogsAgent {
        let aggregator: Arc<Mutex<Aggregator>> = Arc::new(Mutex::new(Aggregator::default()));
        let mut processor = processor::ProcessorType::new(
            Arc::clone(&datadog_config),
            tags_provider,
            LAMBDA_RUNTIME_SLUG.to_string(),
        );

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

            processor.process(event, &cloned_aggregator);
        });

        let dd_api = datadog::Api::new(datadog_config.api_key.clone(), datadog_config.site.clone());
        LogsAgent {
            dd_api,
            aggregator,
            tx,
            join_handle,
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

    fn flush_shutdown(aggregator: &Arc<Mutex<Aggregator>>, dd_api: &datadog::Api) {
        let mut aggregator = aggregator.lock().expect("lock poisoned");
        let mut logs = aggregator.get_batch();
        // It could be an empty JSON array: []
        while logs.len() > 2 {
            dd_api.send(&logs).expect("Failed to send logs to Datadog");
            logs = aggregator.get_batch();
        }
    }

    pub fn shutdown(self) {
        debug!("Shutting down LogsAgent");
        // Dropping this sender to help close the thread
        drop(self.tx);
        match self.join_handle.join() {
            Ok(()) => {
                debug!("LogsAgent Message Receiver thread has been shutdown");
            }
            Err(e) => {
                debug!(
                    "Error shutting down the LogsAgent Message Receiver thread: {:?}",
                    e
                );
            }
        }
        LogsAgent::flush_shutdown(&self.aggregator, &self.dd_api);
    }
}
