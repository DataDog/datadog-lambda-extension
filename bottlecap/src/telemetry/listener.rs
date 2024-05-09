use crate::events;
use crate::telemetry::events::TelemetryEvent;

use std::collections::HashMap;
use std::error::Error;
use std::sync::mpsc::SyncSender;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

use tracing::{debug, error};

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

        let body_start_index = parser.parse_headers(buf)?;
        parser.parse_body(buf, body_start_index)?;

        Ok(parser)
    }

    fn parse_headers(&mut self, buf: &[u8]) -> Result<usize, Box<dyn Error>> {
        let mut last_start = 0;
        let mut i = 0;

        // Ignore method, path, and protocol
        // i + 1 because we are checking next character always
        while i + 1 < buf.len() {
            // '\n\r' indicate the end of every line, the first line
            // is always method, path, and protocol.
            if buf[i] == CR && buf[i + 1] == LR {
                // i + 2 to skip '\n\r' on the next iteration
                last_start = i + 2;
                break;
            }
            i += 1;
        }

        // Parse headers
        i = last_start;
        while i < buf.len() {
            // Here we've reached end of one header
            if buf[i] == CR && buf[i + 1] == LR {
                // Slice the header from the buffer, not using i+1
                // because we don't want to include '\r\n' in the value
                let header = std::str::from_utf8(&buf[last_start..i])?;
                let mut header_parts = header.split(": ");

                if let (Some(key), Some(value)) = (header_parts.next(), header_parts.next()) {
                    self.headers
                        .insert(key.to_string().to_lowercase(), value.to_string());
                } else {
                    error!("Error parsing header, skipping it: {}", header);
                }

                // Check if we reached the end of the headers
                // i + 3 because we are checking next 3 characters, '\r\n\r\n'
                // This indicates the end of the headers, and the start of the body
                if i + 3 < buf.len() && buf[i + 2] == CR && buf[i + 3] == LR {
                    // Set our last_start so we can parse the body
                    last_start = i + 4;
                    break;
                }

                // Skip '\r\n' to start parsing the next header
                last_start = i + 2;
                i = last_start;
                continue;
            }
            i += 1;
        }

        Ok(last_start)
    }

    fn parse_body(&mut self, buf: &[u8], start_index: usize) -> Result<(), Box<dyn Error>> {
        let content_length = match self.headers.get("content-length") {
            Some(length) => length.parse::<usize>()?,
            None => return Err(Box::from("content-length header not found")),
        };

        let end_index = start_index + content_length;

        if end_index > buf.len() {
            return Err(Box::from(
                "content-length header is greater than the buffer length",
            ));
        }

        self.body = std::str::from_utf8(&buf[start_index..end_index])?.to_string();

        Ok(())
    }
}

pub struct TelemetryListener {
    join_handle: std::thread::JoinHandle<()>,
}

pub struct TelemetryListenerConfig {
    pub host: String,
    pub port: u16,
}

impl TelemetryListener {
    pub fn run(
        config: &TelemetryListenerConfig,
        event_bus: SyncSender<events::Event>,
    ) -> Result<TelemetryListener, Box<dyn Error>> {
        let addr = format!("{}:{}", &config.host, &config.port);
        let listener = TcpListener::bind(addr)?;
        let buf: [u8; 262144] = [0; 256 * 1024]; // Using the default limit from AWS

        let join_handle = std::thread::spawn(move || {
            debug!("Initializing Telemetry Listener");

            loop {
                for stream in listener.incoming() {
                    debug!("Received a Telemetry API connection");

                    let cloned_event_bus = event_bus.clone();
                    if let Ok(mut stream) = stream {
                        std::thread::spawn(move || {
                            let r = Self::handle_stream(&mut stream, buf, cloned_event_bus);
                            if let Err(e) = Self::acknowledge_request(stream, r) {
                                error!("Error acknowledging Telemetry request: {:?}", e);
                            }
                        });
                    } else {
                        error!("Error accepting connection");
                    }
                }
            }
        });

        Ok(TelemetryListener { join_handle })
    }

    fn handle_stream(
        stream: &mut impl Read,
        mut buf: [u8; 262144],
        event_bus: SyncSender<events::Event>,
    ) -> Result<(), Box<dyn Error>> {
        // Read into buffer
        #![allow(clippy::unused_io_amount)]
        stream.read(&mut buf)?;

        let p = HttpRequestParser::from_buf(&buf)?;
        let telemetry_events: Vec<TelemetryEvent> = serde_json::from_str(&p.body)?;
        for event in telemetry_events {
            if let Err(e) = event_bus.send(events::Event::Telemetry(event)) {
                error!("Error sending Telemetry event to the event bus: {}", e);
            }
        }

        Ok(())
    }

    #[allow(clippy::unused_io_amount)]
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

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use crate::telemetry::events::{InitPhase, InitType, TelemetryRecord};

    use super::*;

    #[test]
    fn test_parse_headers() {
        let mut parser = HttpRequestParser {
            headers: HashMap::new(),
            body: String::new(),
        };
        let buf = b"GET /path HTTP/1.1\r\nContent-Length: 10\r\nHeader1: Value1\r\n\r\n";
        let result = parser.parse_headers(buf);
        assert!(result.is_ok());
        assert_eq!(parser.headers.len(), 2);
        assert_eq!(
            parser.headers.get("content-length"),
            Some(&"10".to_string())
        );
        assert_eq!(parser.headers.get("header1"), Some(&"Value1".to_string()));
    }

    #[test]
    #[should_panic(expected = "content-length header not found")]
    fn test_parse_headers_no_content_length() {
        let mut parser = HttpRequestParser {
            headers: HashMap::new(),
            body: String::new(),
        };
        let buf = b"GET /path HTTP/1.1\r\nHeader1: Value1\r\n\r\n";
        let body_start_index = parser.parse_headers(buf).unwrap();
        parser.parse_body(buf, body_start_index).unwrap();
    }

    #[test]
    #[should_panic(expected = "content-length header is greater than the buffer length")]
    fn test_parse_headers_wrong_content_length() {
        let mut parser = HttpRequestParser {
            headers: HashMap::new(),
            body: String::new(),
        };
        let buf =
            b"GET /path HTTP/1.1\r\nContent-Length: 56\r\nHeader1: Value1\r\n\r\nHello, World!";
        let body_start_index = parser.parse_headers(buf).unwrap();
        parser.parse_body(buf, body_start_index).unwrap();
    }

    #[test]
    #[should_panic(
        expected = "called `Result::unwrap()` on an `Err` value: ParseIntError { kind: InvalidDigit }"
    )]
    fn test_parse_headers_invalid_content_length() {
        let mut parser = HttpRequestParser {
            headers: HashMap::new(),
            body: String::new(),
        };
        let buf = b"GET /path HTTP/1.1\r\nContent-Length: Bottlecap!\r\nHeader1: Value1\r\n\r\nHello, World!";
        let body_start_index = parser.parse_headers(buf).unwrap();
        parser.parse_body(buf, body_start_index).unwrap();
    }

    #[test]
    fn test_parse_body() {
        let mut parser = HttpRequestParser {
            headers: HashMap::new(),
            body: String::new(),
        };
        parser
            .headers
            .insert("content-length".to_string(), "13".to_string());
        let buf = b"Hello, World!";
        let result = parser.parse_body(buf, 0);
        assert!(result.is_ok());
        assert_eq!(parser.body, "Hello, World!".to_string());
    }

    struct MockTcpStream {
        data: Vec<u8>,
    }

    impl Read for MockTcpStream {
        fn read(&mut self, mut buf: &mut [u8]) -> std::io::Result<usize> {
            let len = std::cmp::min(buf.len(), self.data.len());
            buf.write_all(&self.data[..len])?;
            self.data = self.data.split_off(len);
            Ok(len)
        }
    }

    #[test]
    fn test_handle_stream() {
        let mut stream = MockTcpStream {
            data: "POST /path HTTP/1.1\r\nContent-Length: 335\r\nHeader1: Value1\r\n\r\n[{\"time\":\"2024-04-25T17:35:59.944Z\",\"type\":\"platform.initStart\",\"record\":{\"initializationType\":\"on-demand\",\"phase\":\"init\",\"runtimeVersion\":\"nodejs:20.v22\",\"runtimeVersionArn\":\"arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72\",\"functionName\":\"hello-world\",\"functionVersion\":\"$LATEST\"}}]".to_string().into_bytes(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let buf = [0; 262144];
        let result = TelemetryListener::handle_stream(&mut stream, buf, tx);
        let event = rx.recv().expect("No events received");
        let telemetry_event = match event {
            events::Event::Telemetry(te) => te,
            _ => panic!("Expected Telemetry Event"),
        };

        let expected_time = DateTime::parse_from_rfc3339("2024-04-25T17:35:59.944Z").unwrap();
        assert_eq!(telemetry_event.time, expected_time);
        assert_eq!(telemetry_event.record, TelemetryRecord::PlatformInitStart {
            initialization_type: InitType::OnDemand,
            phase: InitPhase::Init,
            runtime_version: Some("nodejs:20.v22".to_string()), 
            runtime_version_arn: Some("arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72".to_string()), 
        });
        assert!(result.is_ok());
    }

    macro_rules! test_handle_stream_invalid_body {
        ($($name:ident: $value:tt,)*) => {
            $(
                #[test]
                #[should_panic]
                fn $name() {
                    let mut stream = MockTcpStream {
                        data: $value.to_string().into_bytes(),
                    };
                    let (tx, _) = std::sync::mpsc::channel();
                    let buf = [0; 262144];
                    TelemetryListener::handle_stream(&mut stream, buf, tx).unwrap()
                }
            )*
        }
    }

    test_handle_stream_invalid_body! {
        invalid_json: "POST /path HTTP/1.1\r\nContent-Length: 13\r\nHeader1: Value1\r\n\r\nHello, World!",
        empty_json: "POST /path HTTP/1.1\r\nContent-Length: 2\r\nHeader1: Value1\r\n\r\n{}",
        json_array_with_empty_json: "POST /path HTTP/1.1\r\nContent-Length: 4\r\nHeader1: Value1\r\n\r\n[{}]",

    }

    #[test]
    fn test_from_buf() {
        let buf =
            b"GET /path HTTP/1.1\r\nContent-Length: 13\r\nHeader1: Value1\r\n\r\nHello, World!";
        let result = HttpRequestParser::from_buf(buf);
        assert!(result.is_ok());
        let parser = result.unwrap();
        assert_eq!(parser.headers.len(), 2);
        assert_eq!(
            parser.headers.get("content-length"),
            Some(&"13".to_string())
        );
        assert_eq!(parser.headers.get("header1"), Some(&"Value1".to_string()));
        assert_eq!(parser.body, "Hello, World!".to_string());
    }
}
