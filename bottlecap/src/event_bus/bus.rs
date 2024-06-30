use tokio::sync::mpsc::{self, Sender};

use crate::events;

use crate::event_bus::constants::MAX_EVENTS;

#[allow(clippy::module_name_repetitions)]
pub struct EventBus {
    tx: Sender<events::Event>,
    pub rx: mpsc::Receiver<events::Event>,
}

impl EventBus {
    #[must_use]
    pub fn run() -> EventBus {
        let (tx, rx) = mpsc::channel(MAX_EVENTS);
        EventBus { tx, rx }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> Sender<events::Event> {
        self.tx.clone()
    }
}
