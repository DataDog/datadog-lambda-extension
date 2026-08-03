//! In-memory aggregation of DSM consume checkpoints into pipeline-stats buckets,
//! and serialization to the msgpack payload the DSM intake expects.
//!
//! Mirrors the `dd-trace-js` processor: 10-second time buckets keyed by checkpoint
//! hash, each holding `EdgeLatency` / `PathwayLatency` / `PayloadSize` sketches.
//! The serialized payload is msgpack (struct-as-map) and is gzipped by the
//! flusher before being sent to `/api/v0.1/pipeline_stats`.

use std::collections::HashMap;

use serde::Serialize;

use crate::traces::data_streams::checkpoint::Checkpoint;
use crate::traces::data_streams::sketch::DdSketch;

/// Bucket width in nanoseconds (10s), matching the tracer.
const BUCKET_SIZE_NS: u64 = 10_000_000_000;

/// A single checkpoint's accumulated stats within a bucket.
struct StatsPoint {
    hash: u64,
    parent_hash: u64,
    edge_tags: Vec<String>,
    edge_latency: DdSketch,
    pathway_latency: DdSketch,
    payload_size: DdSketch,
}

impl StatsPoint {
    fn new(hash: u64, parent_hash: u64, edge_tags: Vec<String>) -> Self {
        Self {
            hash,
            parent_hash,
            edge_tags,
            edge_latency: DdSketch::new(),
            pathway_latency: DdSketch::new(),
            payload_size: DdSketch::new(),
        }
    }

    fn add(&mut self, edge_latency_ns: u64, pathway_latency_ns: u64, payload_size: f64) {
        #[allow(clippy::cast_precision_loss)]
        let edge_s = edge_latency_ns as f64 / 1e9;
        #[allow(clippy::cast_precision_loss)]
        let pathway_s = pathway_latency_ns as f64 / 1e9;
        self.edge_latency.accept(edge_s);
        self.pathway_latency.accept(pathway_s);
        self.payload_size.accept(payload_size);
    }
}

/// One time bucket: a set of checkpoints keyed by hash.
#[derive(Default)]
struct StatsBucket {
    points: HashMap<u64, StatsPoint>,
}

/// Aggregates DSM checkpoints across invocations until flushed.
pub struct Aggregator {
    service: String,
    env: String,
    tracer_version: String,
    version: String,
    tags: Vec<String>,
    buckets: HashMap<u64, StatsBucket>,
}

impl Aggregator {
    #[must_use]
    pub fn new(
        service: String,
        env: String,
        tracer_version: String,
        version: String,
        tags: Vec<String>,
    ) -> Self {
        Self {
            service,
            env,
            tracer_version,
            version,
            tags,
            buckets: HashMap::new(),
        }
    }

    /// Fold a computed consume checkpoint into the appropriate time bucket.
    pub fn add(&mut self, checkpoint: &Checkpoint, payload_size: f64) {
        let bucket_start = checkpoint.current_ts_ns - (checkpoint.current_ts_ns % BUCKET_SIZE_NS);
        let hash = u64::from_le_bytes(checkpoint.hash);
        let parent_hash = u64::from_le_bytes(checkpoint.parent_hash);

        let bucket = self.buckets.entry(bucket_start).or_default();
        let point = bucket
            .points
            .entry(hash)
            .or_insert_with(|| StatsPoint::new(hash, parent_hash, checkpoint.edge_tags.clone()));
        point.add(
            checkpoint.edge_latency_ns,
            checkpoint.pathway_latency_ns,
            payload_size,
        );
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Drain all buckets and build the msgpack `StatsPayload` (struct-as-map).
    /// Returns `None` when there is nothing to flush.
    #[must_use]
    pub fn take_payload(&mut self) -> Option<Vec<u8>> {
        if self.buckets.is_empty() {
            return None;
        }

        let stats: Vec<StatsBucketSer> = self
            .buckets
            .drain()
            .map(|(start, bucket)| StatsBucketSer {
                start,
                duration: BUCKET_SIZE_NS,
                stats: bucket
                    .points
                    .into_values()
                    .map(|p| StatsPointSer {
                        hash: p.hash,
                        parent_hash: p.parent_hash,
                        edge_tags: p.edge_tags,
                        edge_latency: p.edge_latency.to_proto_bytes(),
                        pathway_latency: p.pathway_latency.to_proto_bytes(),
                        payload_size: p.payload_size.to_proto_bytes(),
                    })
                    .collect(),
                backlogs: Vec::new(),
            })
            .collect();

        let payload = StatsPayloadSer {
            env: self.env.clone(),
            service: self.service.clone(),
            stats,
            tracer_version: self.tracer_version.clone(),
            lang: "rust-extension".to_string(),
            version: self.version.clone(),
            tags: self.tags.clone(),
            // TODO(DSM): Validate resolver-side behavior for extension-produced
            // DD_TAGS / unified tags and whether ProcessTags should also be
            // emitted in addition to top-level Tags.
        };

        match rmp_serde::to_vec_named(&payload) {
            Ok(buf) => Some(buf),
            Err(e) => {
                tracing::warn!("DSM: failed to serialize pipeline stats payload: {e}");
                None
            }
        }
    }
}

#[derive(Serialize)]
struct StatsPayloadSer {
    #[serde(rename = "Env")]
    env: String,
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "Stats")]
    stats: Vec<StatsBucketSer>,
    #[serde(rename = "TracerVersion")]
    tracer_version: String,
    #[serde(rename = "Lang")]
    lang: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Tags")]
    tags: Vec<String>,
}

#[derive(Serialize)]
struct StatsBucketSer {
    #[serde(rename = "Start")]
    start: u64,
    #[serde(rename = "Duration")]
    duration: u64,
    #[serde(rename = "Stats")]
    stats: Vec<StatsPointSer>,
    #[serde(rename = "Backlogs")]
    backlogs: Vec<()>,
}

#[derive(Serialize)]
struct StatsPointSer {
    #[serde(rename = "Hash")]
    hash: u64,
    #[serde(rename = "ParentHash")]
    parent_hash: u64,
    #[serde(rename = "EdgeTags")]
    edge_tags: Vec<String>,
    #[serde(rename = "EdgeLatency", with = "serde_bytes")]
    edge_latency: Vec<u8>,
    #[serde(rename = "PathwayLatency", with = "serde_bytes")]
    pathway_latency: Vec<u8>,
    #[serde(rename = "PayloadSize", with = "serde_bytes")]
    payload_size: Vec<u8>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::traces::data_streams::checkpoint::compute_consume_checkpoint;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct DecodedPayload {
        #[serde(rename = "TracerVersion")]
        tracer_version: String,
        #[serde(rename = "Version")]
        version: String,
        #[serde(rename = "Tags")]
        tags: Vec<String>,
    }

    fn edge_tags() -> Vec<String> {
        vec![
            "direction:in".to_string(),
            "topic:q".to_string(),
            "type:sqs".to_string(),
        ]
    }

    #[test]
    fn empty_aggregator_has_no_payload() {
        let mut agg = Aggregator::new(
            "svc".into(),
            "env".into(),
            "1.0".into(),
            "2.0".into(),
            vec!["team:serverless".into()],
        );
        assert!(agg.is_empty());
        assert!(agg.take_payload().is_none());
    }

    #[test]
    fn aggregates_and_serializes() {
        let mut agg = Aggregator::new(
            "svc".into(),
            "env".into(),
            "1.0".into(),
            "2.0".into(),
            vec!["team:serverless".into(), "region:us-east-1".into()],
        );
        let cp = compute_consume_checkpoint("svc", "env", &edge_tags(), None, 2_000_000_000, None);
        agg.add(&cp, 128.0);
        assert!(!agg.is_empty());

        let payload = agg.take_payload().expect("payload");
        assert!(!payload.is_empty());
        assert!(agg.is_empty());

        assert_eq!(
            payload[0], 0x87,
            "top-level payload must be a 7-entry msgpack map"
        );

        let decoded: DecodedPayload = rmp_serde::from_slice(&payload).expect("decode payload");
        assert_eq!(decoded.tracer_version, "1.0");
        assert_eq!(decoded.version, "2.0");
        assert_eq!(decoded.tags, vec!["team:serverless", "region:us-east-1"]);
    }

    #[test]
    fn same_hash_merges_into_one_point() {
        let mut agg = Aggregator::new(
            "svc".into(),
            "env".into(),
            "1.0".into(),
            "2.0".into(),
            vec!["team:serverless".into()],
        );
        let cp1 = compute_consume_checkpoint("svc", "env", &edge_tags(), None, 2_000_000_000, None);
        let cp2 = compute_consume_checkpoint("svc", "env", &edge_tags(), None, 2_000_000_001, None);
        agg.add(&cp1, 1.0);
        agg.add(&cp2, 1.0);

        assert_eq!(agg.buckets.len(), 1);
        let bucket = agg.buckets.values().next().unwrap();
        assert_eq!(bucket.points.len(), 1);
    }
}
