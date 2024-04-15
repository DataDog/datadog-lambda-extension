use std::sync::mpsc::Sender;

use crate::events::{self, Event, MetricEvent};

pub struct DogStatsD {
    join_handle: std::thread::JoinHandle<()>,
}

impl DogStatsD {
    pub fn run(host: &str, port: u16, event_bus: Sender<events::Event>) -> DogStatsD {
        let addr = format!("{}:{}", host, port);
        let join_handle = std::thread::spawn(move || {
            let socket = std::net::UdpSocket::bind(addr).expect("couldn't bind to address");
            loop {
                let mut buf = [0; 1024]; // todo, do we want to make this dynamic? (not sure)
                let (amt, src) = socket.recv_from(&mut buf).expect("didn't receive data");
                let buf = &mut buf[..amt];
                let msg = std::str::from_utf8(buf).expect("couldn't parse as string");
                log::info!(
                    "received message: {} from {}, sending it to the bus",
                    msg,
                    src
                );
                let dummy_tags = vec!["tagA:valueA".to_string(), "tagB:valueB".to_string()];
                let metric_event = MetricEvent::new("metric_name".to_string(), 1.0, dummy_tags);
                let _ = event_bus.send(Event::Metric(metric_event)); // todo check the result
            }
        });
        DogStatsD { join_handle }
    }

    pub fn shutdown(self) {
        match self.join_handle.join() {
            Ok(_) => {
                log::info!("DogStatsD thread has been shutdown");
            }
            Err(e) => {
                log::error!("Error shutting down the DogStatsD thread: {:?}", e);
            }
        }
    }
}
