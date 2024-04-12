use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::thread;
use tracing::error;

use crate::events;
use events::Event;

struct EventBus {
    tx: Sender<events::Event>,
    rx: Receiver<events::Event>,
}

impl EventBus {
    pub fn run(&self) {
        let (tx, rx) = mpsc::channel::<Event>();
        self.tx = tx;
        self.rx = rx;
        let _ = thread::spawn(move || loop {
            let received = rx.recv();
            if let Ok(event) = received {
                match event {
                    Event::Metric(event) => {
                        println!("Metric event: {:?}", event);
                    }
                    _ => {
                        println!("Other event");
                    }
                }
            } else {
                error!("could not get the event");
            }
        });
    }

    pub fn get_sender_copy(&self) -> Sender<events::Event> {
        self.tx.clone()
    }
}
