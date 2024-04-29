use std::sync::mpsc;
use std::sync::mpsc::Sender;
use std::thread;
use tracing::debug;
use tracing::error;

use crate::events;
use events::Event;

pub struct EventBus {
    tx: Sender<events::Event>,
    join_handle: thread::JoinHandle<()>,
}

impl EventBus {
    pub fn run() -> EventBus {
        let (tx, rx) = mpsc::channel();
        let join_handle = thread::spawn(move || loop {
            let received = rx.recv();
            if let Ok(event) = received {
                match event {
                    Event::Metric(event) => {
                        debug!("Metric event: {:?}", event);
                    }
                    Event::Telemetry(event) => {
                        debug!("Telemetry event: {:?}", event);
                    }
                    _ => {
                        debug!("Other event");
                    }
                }
            } else {
                error!("could not get the event");
            }
        });
        EventBus { tx, join_handle }
    }

    pub fn get_sender_copy(&self) -> Sender<events::Event> {
        self.tx.clone()
    }

    pub fn shutdown(self) {
        match self.join_handle.join() {
            Ok(_) => {
                debug!("EventBus thread has been shutdown");
            }
            Err(e) => {
                error!("Error shutting down the EventBus thread: {:?}", e);
            }
        }
    }
}
