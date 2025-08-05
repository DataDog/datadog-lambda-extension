use tokio::sync::mpsc::{self, Sender};

use crate::{event_bus::constants::MAX_EVENTS, telemetry::events::TelemetryEvent};

mod constants;

#[derive(Debug)]
pub enum Event {
    Telemetry(TelemetryEvent),
    OutOfMemory(i64),
}

#[allow(clippy::module_name_repetitions)]
pub struct EventBus {
    tx: Sender<Event>,
    pub rx: mpsc::Receiver<Event>,
}

impl EventBus {
    #[must_use]
    pub fn run() -> EventBus {
        let (tx, rx) = mpsc::channel(MAX_EVENTS);
        EventBus { tx, rx }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<Event> {
        self.tx.clone()
    }
}
