use crate::telemetry::events::TelemetryEvent;

use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::Sender;

use tracing::{debug, error};

pub struct HttpRequestParser {
    headers: HashMap<String, String>,
    body: String,
}

#[derive(Debug, PartialEq)]
pub enum TcpError {
    Close(String),
    Read(String),
    Write(String),
    Parse(String),
}

impl ToString for TcpError {
    fn to_string(&self) -> String {
        match self {
            TcpError::Close(close) => close.clone(),
            TcpError::Read(read) => read.clone(),
            TcpError::Parse(parse) => parse.clone(),
            TcpError::Write(write) => write.clone(),
        }
    }
}

const CR: u8 = b'\r';
const LR: u8 = b'\n';
/// It is guaranteed that the headers will be less than 256 bytes.
const HEADERS_BUFFER_SIZE: usize = 256;

impl HttpRequestParser {
    /// Create a `HttpRequestParser` from passed `TcpStream`
    ///
    /// # Errors
    ///
    /// Function will error if the stream cannot be read from.
    ///
    /// It will also error if the headers cannot be parsed.
    ///
    /// Or if the body cannot be parsed.
    pub async fn from_stream(stream: &mut TcpStream) -> Result<HttpRequestParser, TcpError> {
        let mut parser = HttpRequestParser {
            headers: HashMap::new(),
            body: String::new(),
        };

        let mut headers_buf = [0u8; HEADERS_BUFFER_SIZE];
        loop {
            // select here
            match stream.read(&mut headers_buf).await {
                Ok(0) => {
                    error!("astuyve Connection closed by client");
                    return Err(TcpError::Close("Connection closed by client".to_string()));
                }
                Ok(_) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => {
                    error!("Error reading from stream: {}", e);
                    return Err(TcpError::Read(e.to_string()));
                }
            }

            let _ = parser.parse_headers(&headers_buf);
            if parser.headers.contains_key("content-length") {
                break;
            }
        }

        let body_start_index = parser.parse_headers(&headers_buf)?;
        let content_length = parser
            .headers
            .get("content-length")
            .expect("infallible")
            .parse::<usize>()
            .map_err(|e| TcpError::Parse(e.to_string()))?;
        let body_bytes_read = headers_buf.len() - body_start_index;
        let missing_body_length = content_length - body_bytes_read;
        let mut body_buf = vec![0u8; missing_body_length];

        stream
            .read_exact(&mut body_buf)
            .await
            .map_err(|e| TcpError::Read(e.to_string()))?;

        let total_bytes_read = headers_buf.len() + missing_body_length;
        let mut buf = vec![0u8; total_bytes_read];
        buf[..headers_buf.len()].copy_from_slice(&headers_buf);
        buf[headers_buf.len()..].copy_from_slice(&body_buf);

        parser.parse_body(&buf, body_start_index)?;

        Ok(parser)
    }

    fn parse_headers(&mut self, buf: &[u8]) -> Result<usize, TcpError> {
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
                let header = std::str::from_utf8(&buf[last_start..i])
                    .map_err(|e| TcpError::Parse(e.to_string()))?;
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

    fn parse_body(&mut self, buf: &[u8], start_index: usize) -> Result<(), TcpError> {
        let content_length = match self.headers.get("content-length") {
            Some(length) => length
                .parse::<usize>()
                .map_err(|e| TcpError::Parse(e.to_string()))?,
            None => {
                return Err(TcpError::Parse(
                    "content-length header not found".to_string(),
                ))
            }
        };

        let end_index = start_index + content_length;

        if end_index > buf.len() {
            return Err(TcpError::Parse(
                "content-length header is greater than the buffer length".to_string(),
            ));
        }

        self.body = std::str::from_utf8(&buf[start_index..end_index])
            .map_err(|e| TcpError::Parse(e.to_string()))?
            .to_string();

        Ok(())
    }
}

#[allow(clippy::module_name_repetitions)]
pub struct TelemetryListener {
    event_bus: Sender<Vec<TelemetryEvent>>,
    cancel_token: tokio_util::sync::CancellationToken,
    listener: TcpListener,
}

pub struct TelemetryListenerConfig {
    pub host: String,
    pub port: u16,
}

impl TelemetryListener {
    /// Run the `TelemetryListener`
    ///
    /// # Errors
    ///
    /// Function will error if the passed address cannot be bound.
    pub async fn new(
        config: &TelemetryListenerConfig,
        event_bus: Sender<Vec<TelemetryEvent>>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<TelemetryListener, String> {
        let addr = format!("{}:{}", &config.host, &config.port);
        let listener = TcpListener::bind(addr).await.map_err(|e| e.to_string())?;

        Ok(TelemetryListener {
            event_bus,
            cancel_token,
            listener,
        })
    }

    pub async fn spin(self) {
        loop {
            match self.listener.accept().await {
                Ok((mut stream, _)) => {
                    debug!("Received a Telemetry API connection");
                    let cloned_event_bus = self.event_bus.clone();
                    tokio::spawn(async move {
                        let _ = Self::handle_stream(&mut stream, cloned_event_bus).await;
                    });
                }
                Err(e) => {
                    error!("Error accepting connection: {:?}", e);
                }
            }
        }
    }

    async fn handle_stream(
        stream: &mut TcpStream,
        event_bus: Sender<Vec<TelemetryEvent>>,
    ) -> Result<(), TcpError> {
        loop {
            // block here
            let p = HttpRequestParser::from_stream(stream).await?;
            let telemetry_events: Vec<TelemetryEvent> =
                serde_json::from_str(&p.body).map_err(|e| TcpError::Parse(e.to_string()))?;
            event_bus
                .send(telemetry_events)
                .await
                .map_err(|e| TcpError::Write(e.to_string()))?; //todo
                                                               //AJ this is cheeky but we should have include the SendError in the enum
            if let Err(e) = Self::acknowledge_request(stream, Ok(())).await {
                error!("Error acknowledging Telemetry request: {:?}", e);
                return Err(TcpError::Write(e));
            }
        }
    }

    async fn acknowledge_request(
        stream: &mut TcpStream,
        request: Result<(), String>,
    ) -> Result<(), String> {
        match request {
            Ok(()) => {
                let _ = stream.write(b"HTTP/1.1 200 OK\r\n\r\n").await;
            }
            Err(_) => {
                let _ = stream.write(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            }
        }
        Ok(())
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
        expected = "called `Result::unwrap()` on an `Err` value: Parse(\"invalid digit found in string\")"
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

    async fn get_stream(data: Vec<u8>) -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel(10000);
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(&data).await.unwrap();
            tx.send(()).await.unwrap(); // Signal that the request has been sent
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        rx.recv().await.unwrap(); // Wait for the signal from the spawned thread

        stream
    }

    #[tokio::test]
    async fn test_handle_stream() {
        let mut stream = get_stream(b"POST /path HTTP/1.1\r\nContent-Length: 335\r\nHeader1: Value1\r\n\r\n[{\"time\":\"2024-04-25T17:35:59.944Z\",\"type\":\"platform.initStart\",\"record\":{\"initializationType\":\"on-demand\",\"phase\":\"init\",\"runtimeVersion\":\"nodejs:20.v22\",\"runtimeVersionArn\":\"arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72\",\"functionName\":\"hello-world\",\"functionVersion\":\"$LATEST\"}}]".to_vec()).await;

        let (tx, mut rx) = tokio::sync::mpsc::channel(3);
        let result = TelemetryListener::handle_stream(&mut stream, tx).await;
        let events = rx.recv().await.expect("No events received");
        let telemetry_event = events.first().expect("failed to get event");

        let expected_time = DateTime::parse_from_rfc3339("2024-04-25T17:35:59.944Z").unwrap();
        assert_eq!(telemetry_event.time, expected_time);
        assert_eq!(telemetry_event.record, TelemetryRecord::PlatformInitStart {
            initialization_type: InitType::OnDemand,
            phase: InitPhase::Init,
            runtime_version: Some("nodejs:20.v22".to_string()),
            runtime_version_arn: Some("arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72".to_string()),
        });
        assert_eq!(
            result.unwrap_err(),
            TcpError::Close("Connection closed by client".to_string())
        );
    }

    macro_rules! test_handle_stream_invalid_body {
        ($($name:ident: $value:tt,)*) => {
            $(
                #[tokio::test]
                #[should_panic]
                async fn $name() {
                    let mut stream = get_stream($value.to_vec()).await;
                    let (tx, _) = tokio::sync::mpsc::channel(4);
                    TelemetryListener::handle_stream(&mut stream, tx).await.unwrap()
                }
            )*
        }
    }

    test_handle_stream_invalid_body! {
        invalid_json: b"POST /path HTTP/1.1\r\nContent-Length: 13\r\nHeader1: Value1\r\n\r\nHello, World!",
        empty_json: b"POST /path HTTP/1.1\r\nContent-Length: 2\r\nHeader1: Value1\r\n\r\n{}",
        json_array_with_empty_json: b"POST /path HTTP/1.1\r\nContent-Length: 4\r\nHeader1: Value1\r\n\r\n[{}]",

    }

    #[tokio::test]
    async fn test_from_stream() {
        let mut stream = get_stream(b"POST /path HTTP/1.1\r\nContent-Length: 335\r\nHeader1: Value1\r\n\r\n[{\"time\":\"2024-04-25T17:35:59.944Z\",\"type\":\"platform.initStart\",\"record\":{\"initializationType\":\"on-demand\",\"phase\":\"init\",\"runtimeVersion\":\"nodejs:20.v22\",\"runtimeVersionArn\":\"arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72\",\"functionName\":\"hello-world\",\"functionVersion\":\"$LATEST\"}}]".to_vec()).await;

        let result = HttpRequestParser::from_stream(&mut stream).await;

        assert!(result.is_ok());
        let parser = result.unwrap();
        assert_eq!(parser.headers.len(), 2);
        assert_eq!(
            parser.headers.get("content-length"),
            Some(&"335".to_string())
        );
        assert_eq!(parser.headers.get("header1"), Some(&"Value1".to_string()));

        assert_eq!(parser.body, "[{\"time\":\"2024-04-25T17:35:59.944Z\",\"type\":\"platform.initStart\",\"record\":{\"initializationType\":\"on-demand\",\"phase\":\"init\",\"runtimeVersion\":\"nodejs:20.v22\",\"runtimeVersionArn\":\"arn:aws:lambda:us-east-1::runtime:da57c20c4b965d5b75540f6865a35fc8030358e33ec44ecfed33e90901a27a72\",\"functionName\":\"hello-world\",\"functionVersion\":\"$LATEST\"}}]".to_string());
    }
}
