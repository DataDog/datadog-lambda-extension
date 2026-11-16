//! Raw log lines, as a function writes them to stdout.

use serde_json::Value;

use crate::fixtures::invocation::{
    FUNCTION_NAME, MEMORY_SIZE_MB, REGION, REQUEST_ID, SERVICE, XRAY_ROOT, log_millis, time,
};

/// Lambda text-format line (about 200 bytes): `<time>\t<request id>\t<level>\t<message>`,
/// timed as the `i`th log line of the invocation.
pub fn text_line(i: usize) -> String {
    let mut s = format!(
        "{}\t{REQUEST_ID}\tINFO\tUser {i} performed checkout action on cart {} with status=ok latency_ms={} region={REGION} path=/api/v1/orders",
        time(log_millis(i)),
        i * 7,
        i % 300
    );
    while s.len() < 200 {
        s.push('x');
    }
    s
}

/// Structured line in the Lambda JSON log format (Powertools-like, about 2 KB), timed as the
/// `i`th log line of the invocation.
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
        "timestamp": time(log_millis(i)),
        "level": "INFO",
        "requestId": REQUEST_ID,
        "message": format!("Processed order {i} for customer cust-{} with \"quoted\" note", i * 3),
        "logger": "orders.handler",
        "service": SERVICE,
        "cold_start": true,
        "function_name": FUNCTION_NAME,
        "function_memory_size": MEMORY_SIZE_MB.to_string(),
        "xray_trace_id": XRAY_ROOT,
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
