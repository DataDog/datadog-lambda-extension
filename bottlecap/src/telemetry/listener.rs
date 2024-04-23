use crate::TELEMETRY_PORT;

use std::{io::Read, net::{self, TcpStream}};

use tracing::{debug, error};

pub struct TelemetryListener {
    join_handle: std::thread::JoinHandle<()>,
}

impl TelemetryListener {
    pub fn run() -> TelemetryListener {
        let addr = format!("0.0.0.0:{}", TELEMETRY_PORT);

        let join_handle = std::thread::spawn(move || {
            let listener = net::TcpListener::bind(&addr).expect("Couldn't bind to address");
            debug!("Initializating Telemetry Listener");

            loop {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            std::thread::spawn(|| {
                                Self::handle_stream(stream); // does it make sense to spawn a new thread per connection?
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

    fn handle_stream(mut stream: TcpStream) {
        let mut buf = [0; 256 * 1024]; // Using the default limit from AWS
        match stream.read(&mut buf) {
            Ok(_) => {
                let message = std::str::from_utf8(&buf).expect("couldn't parse as string");
                debug!("messsage {}", message);
            },
            Err(e) => {
                error!("Error reading from stream: {}", e)
            }
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