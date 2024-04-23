use crate::telemetry::events::TelemetryEvent;
use crate::events;
use crate::TELEMETRY_PORT;

use std::io::Write;
use std::sync::mpsc::Sender;
use std::{io::Read, net::{self, TcpStream}};

use tracing::{debug, error};

pub struct TelemetryListener {
    join_handle: std::thread::JoinHandle<()>,
}

impl TelemetryListener {
    pub fn run(event_bus: Sender<events::Event>) -> TelemetryListener {
        let addr = format!("0.0.0.0:{}", TELEMETRY_PORT);

        let join_handle = std::thread::spawn(move || {
            let listener = net::TcpListener::bind(&addr).expect("Couldn't bind to address");
            debug!("Initializating Telemetry Listener");

            loop {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let cloned_event_bus = event_bus.clone();
                            std::thread::spawn(move || {
                                Self::handle_stream(stream, cloned_event_bus); // does it make sense to spawn a new thread per connection?
                            });
                            
                        },
                        Err(e) => {
                            error!("Error accepting connection: {}", e)
                        }
                    }
                }
            }
        });
        TelemetryListener { join_handle }
    }

    fn handle_stream(mut stream: TcpStream, event_bus: Sender<events::Event>) {
        debug!("Received a connection!");
        let mut buf = [0; 256 * 1024]; // Using the default limit from AWS
        // TODO: Optimize this algorithm
        match stream.read(&mut buf) {
            Ok(_) => {
                // Not an optimal way to find where our message ends
                // Could be improved using the content-lenght of the request
                let end_index = buf.iter().position(|&r| r == 0).unwrap_or(buf.len());
                // Is there a better way to do this? Sliding window?
                let buf_str = std::str::from_utf8(&buf[..end_index]).expect("Couldn't parse as string");

                // We only want the left part of the string, assuming it always
                // contains data
                let body = buf_str.split("\r\n\r\n").collect::<Vec<&str>>()[1];
                
                let telemetry_events: Vec<TelemetryEvent> = serde_json::from_str(body).expect("Couldn't parse telemetry event");
                for event in telemetry_events {
                    let _ = event_bus.send(events::Event::Telemetry(event));
                }
            },
            Err(e) => {
                error!("Error reading from stream: {}", e)
            }
        }

        let response = b"HTTP/1.1 200 OK\r\n\r\n";
        match stream.write(response) {
            Ok(_) => debug!("Acknowledge data receive"),
            Err(e) => error!("Error sending response: {}", e),
        }
    }

    pub fn shutdown(self) {
        match self.join_handle.join() {
            Ok(_) => {
                debug!("Telemetry Listener thread has been shutdown");
            }
            Err(e) => {
                error!("Error shutting down the Telemetry Listener thread: {:?}", e);
            }
        }
    }
}