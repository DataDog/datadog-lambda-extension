// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error};

use libdd_common::http_common;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::stats_utils;

use crate::http::extract_request_body;
use crate::traces::trace_agent::MAX_CONTENT_LENGTH;

#[async_trait]
pub trait StatsProcessor {
    /// Deserializes trace stats from a request body and sends them through
    /// the provided tokio mpsc Sender.
    async fn process_stats(
        &self,
        req: Request,
        tx: Sender<pb::ClientStatsPayload>,
    ) -> Result<Response, Box<dyn std::error::Error + Send + Sync>>;
}

#[derive(Clone, Copy)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessStatsProcessor {}

#[async_trait]
impl StatsProcessor for ServerlessStatsProcessor {
    async fn process_stats(
        &self,
        req: Request,
        tx: Sender<pb::ClientStatsPayload>,
    ) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
        debug!("Received trace stats to process");
        let (parts, body) = match extract_request_body(req).await {
            Ok(r) => r,
            Err(e) => {
                let error_msg = format!("Error extracting request body: {e}");
                error!("{}", error_msg);
                return Ok((StatusCode::BAD_REQUEST, error_msg).into_response());
            }
        };

        if let Some(content_length) = parts.headers.get("content-length")
            && let Ok(length_str) = content_length.to_str()
            && let Ok(length) = length_str.parse::<usize>()
            && length > MAX_CONTENT_LENGTH
        {
            let error_msg = format!(
                "Content-Length {length} exceeds maximum allowed size {MAX_CONTENT_LENGTH}"
            );
            error!("{}", error_msg);
            return Ok((StatusCode::PAYLOAD_TOO_LARGE, error_msg).into_response());
        }

        // deserialize trace stats from the request body, convert to protobuf structs (see
        // trace-protobuf crate)
        let mut stats: pb::ClientStatsPayload =
            match stats_utils::get_stats_from_request_body(http_common::Body::from_bytes(body))
                .await
            {
                Ok(result) => result,
                Err(err) => {
                    let error_msg =
                        format!("Error deserializing trace stats from request body: {err}");
                    error!("{}", error_msg);
                    return Ok((StatusCode::INTERNAL_SERVER_ERROR, error_msg).into_response());
                }
            };

        // An empty stats payload (e.g. an empty msgpack map) has nothing to buffer, so
        // treat it as a harmless no-op instead of indexing into an empty vec (which panics).
        let Some(first_bucket) = stats.stats.first_mut() else {
            debug!("Received empty trace stats payload; nothing to aggregate.");
            return Ok((StatusCode::ACCEPTED, "Empty stats payload; nothing to aggregate.")
                .into_response());
        };

        let start = SystemTime::now();
        let timestamp = start
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        first_bucket.start = if let Ok(result) = u64::try_from(timestamp) {
            result
        } else {
            let error_msg = "Error converting timestamp to u64";
            error!("{}", error_msg);
            return Ok((StatusCode::INTERNAL_SERVER_ERROR, error_msg).into_response());
        };

        // send trace stats payload to our stats aggregator
        match tx.send(stats).await {
            Ok(()) => {
                debug!("Successfully buffered stats to be aggregated.");
                Ok((
                    StatusCode::ACCEPTED,
                    "Successfully buffered stats to be aggregated.",
                )
                    .into_response())
            }
            Err(err) => {
                let error_msg = format!("Error sending stats to the stats aggregator: {err}");
                error!("{}", error_msg);
                Ok((StatusCode::INTERNAL_SERVER_ERROR, error_msg).into_response())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn empty_stats_payload_does_not_panic() {
        // An empty msgpack map (0x80) deserializes to a ClientStatsPayload with an empty
        // `stats` vec. Indexing `stats.stats[0]` used to panic here; assert we return 202
        // Accepted instead and buffer nothing.
        let req = Request::builder()
            .body(Body::from(vec![0x80u8]))
            .expect("failed to build request");

        let (tx, mut rx) = mpsc::channel(1);
        let response = ServerlessStatsProcessor {}
            .process_stats(req, tx)
            .await
            .expect("process_stats returned an error");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert!(
            rx.try_recv().is_err(),
            "no stats payload should be buffered for an empty request"
        );
    }
}
