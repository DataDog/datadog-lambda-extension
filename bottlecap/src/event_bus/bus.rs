use std::sync::mpsc;
use std::sync::mpsc::Sender;

use crate::events;

pub struct EventBus {
    tx: Sender<events::Event>,
    pub rx: mpsc::Receiver<events::Event>,
}

impl EventBus {
    pub fn run() -> EventBus {
        let (tx, rx) = mpsc::channel();
        EventBus { tx, rx }
    }

    pub fn get_sender_copy(&self) -> Sender<events::Event> {
        self.tx.clone()
    }
}
