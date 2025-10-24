use tokio::sync::mpsc::{self, Sender};

use crate::{event_bus::constants::MAX_EVENTS, extension::telemetry::events::TelemetryEvent};

mod constants;

#[derive(Debug)]
pub enum Event {
    Telemetry(TelemetryEvent),
    OutOfMemory(i64),
    Tombstone,
}

#[allow(clippy::module_name_repetitions)]
pub struct EventBus {
    pub rx: mpsc::Receiver<Event>,
}

impl EventBus {
    #[must_use]
    pub fn run() -> (EventBus, Sender<Event>) {
        let (tx, rx) = mpsc::channel(MAX_EVENTS);
        let event_bus = EventBus { rx };
        (event_bus, tx)
    }
}
