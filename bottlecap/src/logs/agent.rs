use std::sync::mpsc::{Sender, SyncSender};
use std::sync::{mpsc, Arc};
use std::thread;

use tracing::debug;

use crate::events::Event;
use crate::logs::{aggregator::Aggregator, datadog, processor::LogsProcessor};
use crate::tags;
use crate::telemetry::events::TelemetryEvent;
use crate::{config, LAMBDA_RUNTIME_SLUG};

#[allow(clippy::module_name_repetitions)]
pub struct LogsAgent {
    dd_api: datadog::Api,
    aggregator: Arc<Aggregator>,
    tx: Sender<Vec<TelemetryEvent>>,
    join_handle: std::thread::JoinHandle<()>,
}

impl LogsAgent {
    #[must_use]
    pub fn run(
        tags_provider: Arc<tags::provider::Provider>,
        datadog_config: Arc<config::Config>,
        event_bus: SyncSender<Event>,
    ) -> LogsAgent {
        let aggregator = Arc::new(Aggregator::default());
        let mut processor = LogsProcessor::new(
            Arc::clone(&datadog_config),
            tags_provider,
            event_bus,
            LAMBDA_RUNTIME_SLUG.to_string(),
        );

        let clone_aggregator = Arc::clone(&aggregator);

        let (tx, rx) = mpsc::channel::<Vec<TelemetryEvent>>();
        let join_handle = thread::spawn(move || loop {
            let received = rx.recv();
            // TODO(duncanista): we might need to create a Event::Shutdown
            // to signal shutdown and make it easier to handle any floating events
            let Ok(events) = received else {
                debug!("Failed to received event in Logs Agent");
                break;
            };

            processor.process(events, clone_aggregator.clone());
        });

        let dd_api = datadog::Api::new(datadog_config.api_key.clone(), datadog_config.site.clone());
        LogsAgent {
            dd_api,
            aggregator,
            tx,
            join_handle,
        }
    }

    pub fn flush(&self) {
        LogsAgent::flush_internal(self.aggregator.clone(), &self.dd_api);
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<Vec<TelemetryEvent>> {
        self.tx.clone()
    }

    fn flush_internal(aggregator: Arc<Aggregator>, dd_api: &datadog::Api) {
        let logs = aggregator.get_batch();
        dd_api.send(&logs).expect("Failed to send logs to Datadog");
        // let logs = *aggregator.get_batch();
        // dd_api.send(&logs).expect("Failed to send logs to Datadog");

        // debug!("[agent][flush_internal] Acquiring lock");
        // let guard = aggregator.try_lock();
        // match guard {
        //     Ok(mut guard) => {
        //         debug!("[agent][flush_internal] Lock acquired");
        //         let logs = guard.get_batch();
        //         drop(guard);
        //         dd_api.send(&logs).expect("Failed to send logs to Datadog");
        //     }
        //     Err(e) => match e {
        //         std::sync::TryLockError::WouldBlock => {
        //             debug!("[agent][flush_internal] Lock is already acquired, blocking");
        //             let mut guard = aggregator.lock().expect("lock poisoned");
        //             let logs = guard.get_batch();
        //             drop(guard);
        //             dd_api.send(&logs).expect("Failed to send logs to Datadog");
        //         }
        //         std::sync::TryLockError::Poisoned(_) => {
        //             debug!("[agent][flush_internal] Lock is poisoned");
        //         }
        //     },
        // }
    }
    fn flush_shutdown(aggregator: Arc<Aggregator>, dd_api: &datadog::Api) {
        let mut logs = aggregator.get_batch();
        while logs.len() > 2 {
            dd_api.send(&logs).expect("Failed to send logs to Datadog");
            logs = aggregator.get_batch();
        }
        // let mut guard = aggregator.lock().expect("lock poisoned");
        // let mut logs = guard.get_batch();
        // drop(guard);
        // // It could be an empty JSON array: []
        // while logs.len() > 2 {
        //     dd_api.send(&logs).expect("Failed to send logs to Datadog");
        //     guard = aggregator.lock().expect("lock poisoned");
        //     logs = guard.get_batch();
        //     drop(guard);
        // }
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
        LogsAgent::flush_shutdown(self.aggregator.clone(), &self.dd_api);
    }
}
