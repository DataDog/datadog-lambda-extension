//! Raw log lines, as a function writes them to stdout.

use serde_json::Value;

use crate::fixtures::telemetry::REQUEST_ID;

/// Lambda text-format line (about 200 bytes): `<time>\t<request id>\t<level>\t<message>`.
pub fn text_line(i: usize) -> String {
    let mut s = format!(
        "2026-10-27T12:00:00.{:03}Z\t{REQUEST_ID}\tINFO\tUser {i} performed checkout action on cart {} with status=ok latency_ms={} region=us-east-1 path=/api/v1/orders",
        i % 1000,
        i * 7,
        i % 300
    );
    while s.len() < 200 {
        s.push('x');
    }
    s
}

/// Structured line in the Lambda JSON log format (Powertools-like, about 2 KB).
pub fn json_object(i: usize) -> Value {
    let items: Vec<Value> = (0..12)
        .map(|k| {
            serde_json::json!({
                "sku": format!("SKU-{i}-{k}"),
                "qty": k,
                "price": 19.99,
                "name": "Widget deluxe edition",
            })
        })
        .collect();
    serde_json::json!({
        "timestamp": "2026-10-27T12:00:00.123Z",
        "level": "INFO",
        "requestId": REQUEST_ID,
        "message": format!("Processed order {i} for customer cust-{} with \"quoted\" note", i * 3),
        "logger": "orders.handler",
        "service": "orders",
        "cold_start": false,
        "function_name": "orders-prod-handler",
        "function_memory_size": "128",
        "xray_trace_id": "1-5759e988-bd862e3fe1be46a994272793",
        "items": items,
    })
}

/// [`json_object`] serialized on one line.
pub fn json_line(i: usize) -> String {
    serde_json::to_string(&json_object(i)).expect("serialize json log line")
}

/// One runtime OOM line per supported runtime, in the order of `OOM_ERRORS` in
/// `src/logs/lambda/processor.rs`.
pub const OOM_LINES: [&str; 7] = [
    "fatal error: runtime: out of memory",
    "Exception in thread \"main\" java.lang.OutOfMemoryError: Java heap space",
    "FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory",
    "Runtime exited with error: signal: killed Runtime.ExitError",
    "MemoryError",
    "failed to allocate memory (NoMemoryError)",
    "Unhandled exception. System.OutOfMemoryException: Insufficient memory to continue the execution of the program.",
];

/// A text line padded to `len` bytes, for size-sensitive scans such as the OOM check.
pub fn text_line_of_len(i: usize, len: usize) -> String {
    let mut s = text_line(i);
    while s.len() < len {
        s.push_str(" some more log text here");
    }
    s.truncate(len);
    s
}
