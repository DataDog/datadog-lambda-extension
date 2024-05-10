use std::sync::mpsc::{self, SyncSender};

use crate::events;

use crate::event_bus::constants::MAX_EVENTS;

#[allow(clippy::module_name_repetitions)]
pub struct EventBus {
    tx: SyncSender<events::Event>,
    pub rx: mpsc::Receiver<events::Event>,
}

impl EventBus {
    #[must_use]
    pub fn run() -> EventBus {
        let (tx, rx) = mpsc::sync_channel(MAX_EVENTS);
        EventBus { tx, rx }
    }

    #[must_use]
    pub fn get_sender_copy(&self) -> SyncSender<events::Event> {
        self.tx.clone()
    }
}
