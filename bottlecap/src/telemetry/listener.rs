use crate::events;
use crate::telemetry::events::TelemetryEvent;
use crate::TELEMETRY_PORT;

use std::collections::HashMap;
use std::error::Error;
use std::sync::mpsc::Sender;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

use tracing::debug;

pub struct HttpRequestParser {
    headers: HashMap<String, String>,
    body: String,
}

const CR: u8 = b'\r';
const LR: u8 = b'\n';

impl HttpRequestParser {
    pub fn from_buf(buf: &[u8]) -> Result<HttpRequestParser, Box<dyn Error>> {
        let mut parser = HttpRequestParser {
            headers: HashMap::new(),
            body: String::new(),
        };

        let body_start_index = parser.parse_headers(&buf)?;
        let _ = parser.parse_body(&buf, body_start_index);

        Ok(parser)
    }

    fn parse_headers(&mut self, buf: &[u8]) -> Result<usize, Box<dyn Error>> {
        let mut last_start = 0;
        let mut i = 0;

        // Ignore method, path, and protocol
        while i + 1 < buf.len() {
            if buf[i] == CR && buf[i + 1] == LR {
                last_start = i + 2;
                break;
            }
            i += 1;
        }

        // Parse headers
        i = last_start;
        while i < buf.len() {
            if buf[i] == CR && buf[i + 1] == LR {
                let header = std::str::from_utf8(&buf[last_start..i])?;
                let mut header_parts = header.split(": ");
                let key: &str = header_parts.next().unwrap();
                let value = header_parts.next().unwrap();
                self.headers
                    .insert(key.to_string().to_lowercase(), value.to_string());

                // Check if we reached the end of the headers
                if i + 3 < buf.len() && buf[i + 2] == CR && buf[i + 3] == LR {
                    last_start = i + 4;
                    break;
                }

                last_start = i + 2;
                i = last_start;
                continue;
            }
            i += 1;
        }

        Ok(last_start)
    }

    fn parse_body(&mut self, buf: &[u8], start_index: usize) -> Result<(), Box<dyn Error>> {
        let end_index = start_index
            + self
                .headers
                .get("content-length")
                .unwrap()
                .parse::<usize>()?;

        self.body = std::str::from_utf8(&buf[start_index..end_index])?.to_string();

        Ok(())
    }
}

pub struct TelemetryListener {
    join_handle: std::thread::JoinHandle<()>,
}

impl TelemetryListener {
    pub fn run(event_bus: Sender<events::Event>) -> Result<TelemetryListener, Box<dyn Error>> {
        let addr = format!("0.0.0.0:{}", TELEMETRY_PORT);
        let listener = TcpListener::bind(&addr)?;
        let buf: [u8; 262144] = [0; 256 * 1024]; // Using the default limit from AWS

        let join_handle = std::thread::spawn(move || {
            debug!("Initializating Telemetry Listener");

            loop {
                for stream in listener.incoming() {
                    debug!("Received a Telemetry API connection");

                    let cloned_event_bus = event_bus.clone();
                    let stream = stream.unwrap();
                    // TODO: revisit if it makes sense to spawn a new thread per connection.
                    std::thread::spawn(move || {
                        let r = Self::handle_stream(&stream, buf, cloned_event_bus);
                        let _ = Self::acknowledge_request(stream, r);
                    });
                }
            }
        });

        Ok(TelemetryListener { join_handle })
    }

    fn handle_stream(
        mut stream: &TcpStream,
        mut buf: [u8; 262144],
        event_bus: Sender<events::Event>,
    ) -> Result<(), Box<dyn Error>> {
        // Read into buffer
        stream.read(&mut buf)?;

        let p = HttpRequestParser::from_buf(&buf)?;
        let telemetry_events: Vec<TelemetryEvent> = serde_json::from_str(&p.body)?;
        for event in telemetry_events {
            let _ = event_bus.send(events::Event::Telemetry(event));
        }

        Ok(())
    }

    fn acknowledge_request(
        mut stream: TcpStream,
        request: Result<(), Box<dyn Error>>,
    ) -> Result<(), Box<dyn Error>> {
        match request {
            Ok(_) => {
                stream.write(b"HTTP/1.1 200 OK\r\n\r\n")?;
            }
            Err(_) => {
                stream.write(b"HTTP/1.1 400 Bad Request\r\n\r\n")?;
            }
        }
        Ok(())
    }

    pub fn shutdown(self) {
        match self.join_handle.join() {
            Ok(_) => {
                debug!("Telemetry Listener thread has been shutdown");
            }
            Err(e) => {
                debug!("Error shutting down the Telemetry Listener thread: {:?}", e);
            }
        }
    }
}
