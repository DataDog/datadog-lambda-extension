use std::sync::mpsc;
use std::sync::mpsc::Sender;
use std::thread;
use tracing::error;

use crate::events;
use events::Event;

pub struct EventBus {
    tx: Option<Sender<events::Event>>,
}

impl EventBus {
    pub fn new() -> EventBus {
        EventBus { tx: None }
    }
    pub fn run(&mut self) {
        let (tx, rx) = mpsc::channel::<Event>();
        self.tx = Some(tx);
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
        match &self.tx {
            Some(sender) => sender.clone(),
            None => panic!("The event bus needs to be initialized first"),
        }
    }
}
