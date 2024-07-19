// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hyper::{http, Body, Request, Response, StatusCode};
use tokio::sync::mpsc::Sender;
use tracing::debug;

use datadog_trace_protobuf::pb;
use datadog_trace_utils::stats_utils;

use super::trace_agent::MAX_CONTENT_LENGTH;
use datadog_trace_mini_agent::http_utils::{self, log_and_create_http_response};

#[async_trait]
pub trait StatsProcessor {
    /// Deserializes trace stats from a hyper request body and sends them through
    /// the provided tokio mpsc Sender.
    async fn process_stats(
        &self,
        req: Request<Body>,
        tx: Sender<pb::ClientStatsPayload>,
    ) -> http::Result<Response<Body>>;
}

#[derive(Clone, Copy)]
#[allow(clippy::module_name_repetitions)]
pub struct ServerlessStatsProcessor {}

#[async_trait]
impl StatsProcessor for ServerlessStatsProcessor {
    async fn process_stats(
        &self,
        req: Request<Body>,
        tx: Sender<pb::ClientStatsPayload>,
    ) -> http::Result<Response<Body>> {
        debug!("Received trace stats to process");
        let (parts, body) = req.into_parts();

        if let Some(response) = http_utils::verify_request_content_length(
            &parts.headers,
            MAX_CONTENT_LENGTH,
            "Error processing trace stats",
        ) {
            return response;
        }

        // deserialize trace stats from the request body, convert to protobuf structs (see
        // trace-protobuf crate)
        let mut stats: pb::ClientStatsPayload =
            match stats_utils::get_stats_from_request_body(body).await {
                Ok(result) => result,
                Err(err) => {
                    return log_and_create_http_response(
                        &format!("Error deserializing trace stats from request body: {err}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

        let start = SystemTime::now();
        let timestamp = start
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        stats.stats[0].start = match u64::try_from(timestamp) {
            Ok(result) => result,
            Err(_) => {
                return log_and_create_http_response(
                    "Error converting timestamp to u64",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        // send trace payload to our trace flusher
        match tx.send(stats).await {
            Ok(()) => {
                return log_and_create_http_response(
                    "Successfully buffered stats to be flushed.",
                    StatusCode::ACCEPTED,
                );
            }
            Err(err) => {
                return log_and_create_http_response(
                    &format!("Error sending stats to the stats flusher: {err}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    }
}
