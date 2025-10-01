use axum::{Router, extract::Request, http::StatusCode, routing::post};
use flate2::read::GzDecoder;
use serde_json::Value;
use std::io::Read;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing::debug;

pub async fn start_listener() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = Router::new()
        // Endpoints that return JSON responses
        .route("/api/beta/sketches", post(handle_sketches))
        .route("/api/v2/logs", post(handle_logs))
        .route("/api/v0.2/traces", post(handle_traces_v02))
        .route("/api/v0.4/traces", post(handle_traces_v04))
        // Endpoints that do nothing
        .route("/api/v0.2/stats", post(handle_noop))
        .route("/api/v1/check_run", post(handle_noop))
        .route("/api/v1/series", post(handle_noop))
        .fallback(fallback_handler);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3333));
    let listener = TcpListener::bind(addr).await?;

    debug!("Listener server starting on {}", addr);

    axum::serve(listener, router).await?;

    Ok(())
}

fn decompress_data(bytes: &[u8], encoding: Option<&str>) -> Result<Vec<u8>, String> {
    match encoding {
        Some("zstd") => {
            zstd::decode_all(bytes).map_err(|e| format!("Zstd decompression failed: {}", e))
        }
        Some("gzip") => {
            let mut decoder = GzDecoder::new(bytes);
            let mut decompressed = Vec::new();
            decoder
                .read_to_end(&mut decompressed)
                .map_err(|e| format!("Gzip decompression failed: {}", e))?;
            Ok(decompressed)
        }
        _ => Ok(bytes.to_vec()), // No compression
    }
}

async fn handle_request_with_decompression(request: Request, endpoint_name: &str) -> StatusCode {
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default();

    // Print headers
    println!("[{}] Headers:", endpoint_name);
    for (key, value) in &parts.headers {
        println!("  {}: {}", key, value.to_str().unwrap_or("<non-utf8>"));
    }

    // Get content encoding
    let content_encoding = parts
        .headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok());

    // Decompress if needed
    let decompressed_bytes = match decompress_data(&bytes, content_encoding) {
        Ok(data) => data,
        Err(e) => {
            println!("[{}] Decompression error: {}", endpoint_name, e);
            bytes.to_vec()
        }
    };

    // Print the decompressed bytes
    println!("[{}] Body: {:?}", endpoint_name, decompressed_bytes);

    StatusCode::OK
}

async fn read_request_body(request: Request) -> Result<Value, String> {
    let (_parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|e| format!("Failed to read body: {}", e))?;

    // Try to parse as JSON directly from bytes first
    if let Ok(value) = serde_json::from_slice(&bytes) {
        return Ok(value);
    }

    // If that fails, try UTF-8 conversion
    let json_str =
        String::from_utf8(bytes.to_vec()).map_err(|e| format!("Invalid UTF-8: {}", e))?;

    serde_json::from_str(&json_str).map_err(|e| format!("Invalid JSON: {}", e))
}

async fn handle_sketches(request: Request) -> StatusCode {
    handle_request_with_decompression(request, "SKETCHES").await
}

async fn handle_logs(request: Request) -> StatusCode {
    handle_request_with_decompression(request, "LOGS").await
}

async fn handle_traces_v02(request: Request) -> StatusCode {
    handle_request_with_decompression(request, "TRACES_V02").await
}

async fn handle_traces_v04(request: Request) -> StatusCode {
    handle_request_with_decompression(request, "TRACES_V04").await
}

async fn fallback_handler(request: Request) -> StatusCode {
    match read_request_body(request).await {
        Ok(payload) => {
            debug!("Received fallback request: {:?}", payload);
        }
        Err(e) => {
            debug!("Fallback handler error reading request: {}", e);
        }
    }
    StatusCode::OK
}

async fn handle_noop(_request: Request) -> StatusCode {
    debug!("Received noop request");
    StatusCode::OK
}
