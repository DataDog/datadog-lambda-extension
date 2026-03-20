// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io::Write as _;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;

use crate::config;
use crate::lifecycle::invocation::processor::S_TO_MS;
use crate::traces::http_client::HttpClient;
use crate::traces::stats_aggregator::StatsAggregator;
use dogstatsd::api_key::ApiKeyFactory;
use libdd_common::Endpoint;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::{config_utils::trace_stats_url, stats_utils};
use serde::Serialize;
use serde::Serializer;
use tracing::{debug, error};

fn serialize_bytes<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
    if s.is_human_readable() {
        s.serialize_str(&BASE64.encode(bytes))
    } else {
        s.serialize_bytes(bytes)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StatsPayload<'a> {
    agent_env: &'a str,
    agent_version: &'a str,
    #[serde(rename = "Stats")]
    stats: Vec<ClientStatsPayload<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ClientStatsPayload<'a> {
    env: &'a str,
    version: &'a str,
    lang: &'a str,
    #[serde(rename = "Stats")]
    stats: Vec<ClientStatsBucket<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ClientStatsBucket<'a> {
    start: u64,
    duration: u64,
    #[serde(rename = "Stats")]
    stats: Vec<ClientGroupedStats<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ClientGroupedStats<'a> {
    service: &'a str,
    name: &'a str,
    resource: &'a str,
    #[serde(rename = "HTTPStatusCode")]
    http_status_code: u32,
    #[serde(rename = "Type")]
    r#type: &'a str,
    hits: u64,
    duration: u64,
    #[serde(serialize_with = "serialize_bytes")]
    ok_summary: &'a [u8],
    #[serde(serialize_with = "serialize_bytes")]
    error_summary: &'a [u8],
    top_level_hits: u64,
    span_kind: &'a str,
    is_trace_root: i32,
}

fn serialize_payload(payload: &StatsPayload<'_>) -> anyhow::Result<Vec<u8>> {
    let msgpack = rmp_serde::to_vec_named(payload)?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&msgpack)?;
    encoder.finish().map_err(|e| anyhow::anyhow!("Error compressing stats payload: {e}"))
}

pub struct StatsFlusher {
    aggregator: Arc<Mutex<StatsAggregator>>,
    config: Arc<config::Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    endpoint: OnceCell<Endpoint>,
    http_client: HttpClient,
}

impl StatsFlusher {
    #[must_use]
    pub fn new(
        api_key_factory: Arc<ApiKeyFactory>,
        aggregator: Arc<Mutex<StatsAggregator>>,
        config: Arc<config::Config>,
        http_client: HttpClient,
    ) -> Self {
        StatsFlusher {
            aggregator,
            config,
            api_key_factory,
            endpoint: OnceCell::new(),
            http_client,
        }
    }

    /// Flushes stats to the Datadog trace stats intake.
    ///
    /// Returns `None` on success, or `Some(failed_stats)` if the flush failed and should be retried.
    pub async fn send(
        &self,
        stats: Vec<pb::ClientStatsPayload>,
    ) -> Option<Vec<pb::ClientStatsPayload>> {
        if stats.is_empty() {
            return None;
        }

        let payload = StatsPayload {
            agent_env: "rey",
            agent_version: "6.0.0",
            stats: stats.iter().map(|csp| ClientStatsPayload {
                env: "rey",
                version: "1",
                lang: "python",
                stats: csp.stats.iter().map(|csb| ClientStatsBucket {
                    start: csb.start,
                    duration: csb.duration,
                    stats: vec! [
                        ClientGroupedStats {
                            service: "rey-python-lambda",
                            name: "flask.request",
                            resource: "GET /",
                            http_status_code: 200,
                            r#type: "web",
                            // count of name=aws.lambda hits, there should only be one
                            hits: csb.stats.iter().filter(|cs| cs.name == "aws.lambda").map(|cs| cs.hits).sum(),
                            duration: csb.duration,
                            ok_summary: "CgkJ/UqBWr9S8D8SDRIIAAAAAAAA8D8Yzg0aAA==".as_bytes(),
                            error_summary: "CgkJ/UqBWr9S8D8SABoA".as_bytes(),
                            top_level_hits: csb.stats.iter().filter(|cs| cs.name == "aws.lambda").map(|cs| cs.top_level_hits).sum(),
                            span_kind: "server",
                            is_trace_root: 2,
                        },
                    ],
                }).collect(),
            }).collect(),
        };

        match serde_json::to_string(&payload) {
            Ok(json) => debug!("STATS | DEBUG: stats payload being sent: {json}"),
            Err(e) => debug!("STATS | DEBUG: could not serialize stats payload for logging: {e}"),
        }

        let Some(api_key) = self.api_key_factory.get_api_key().await else {
            error!("STATS | Skipping flushing stats: Failed to resolve API key");
            // No API key means we can't send - don't retry as it won't help
            return None;
        };

        let api_key_clone = api_key.clone();
        let endpoint = self
            .endpoint
            .get_or_init({
                move || async move {
                    let stats_url = trace_stats_url(&self.config.site);
                    Endpoint {
                        url: hyper::Uri::from_str(&stats_url)
                            .expect("can't make URI from stats url, exiting"),
                        api_key: Some(api_key_clone.into()),
                        timeout_ms: self.config.flush_timeout * S_TO_MS,
                        test_token: None,
                        use_system_resolver: false,
                    }
                }
            })
            .await;

        let serialized_stats_payload = match serialize_payload(&payload) {
            Ok(res) => res,
            Err(err) => {
                // Serialization errors are permanent - data is malformed, don't retry
                error!("STATS | Failed to serialize stats payload, dropping stats: {err}");
                return None;
            }
        };

        let stats_url = trace_stats_url(&self.config.site);

        let start = std::time::Instant::now();

        let resp = stats_utils::send_stats_payload_with_client(
            serialized_stats_payload,
            endpoint,
            api_key.as_str(),
            Some(&self.http_client),
        )
        .await;
        let elapsed = start.elapsed();
        debug!(
            "STATS | Stats request to {} took {} ms",
            stats_url,
            elapsed.as_millis()
        );
        match resp {
            Ok(()) => {
                debug!("STATS | Successfully flushed stats");
                None
            }
            Err(e) => {
                // Network/server errors are temporary - return stats for retry
                error!("STATS | Error sending stats: {e:?}");
                Some(stats)
            }
        }
    }

    /// Flushes stats from the aggregator.
    ///
    /// Returns `None` on success, or `Some(failed_stats)` if any flush failed and should be retried.
    /// If `failed_stats` is provided, it will attempt to send those first before fetching new stats.
    pub async fn flush(
        &self,
        force_flush: bool,
        failed_stats: Option<Vec<pb::ClientStatsPayload>>,
    ) -> Option<Vec<pb::ClientStatsPayload>> {
        let mut all_failed: Vec<pb::ClientStatsPayload> = Vec::new();

        // First, retry any previously failed stats
        if let Some(retry_stats) = failed_stats
            && !retry_stats.is_empty()
        {
            debug!(
                "STATS | Retrying {} previously failed stats",
                retry_stats.len()
            );
            if let Some(still_failed) = self.send(retry_stats).await {
                all_failed.extend(still_failed);
            }
        }

        // Then flush new stats from the aggregator
        let mut guard = self.aggregator.lock().await;
        let mut stats = guard.get_batch(force_flush).await;
        while !stats.is_empty() {
            if let Some(failed) = self.send(stats).await {
                all_failed.extend(failed);
            }
            stats = guard.get_batch(force_flush).await;
        }

        if all_failed.is_empty() {
            None
        } else {
            Some(all_failed)
        }
    }
}
