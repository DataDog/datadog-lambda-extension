use crate::config::Config;
use crate::lifecycle::invocation::processor::S_TO_NS_U64;
use crate::tags::provider::Provider as TagProvider;
use crate::traces::stats_agent::StatsEvent;
/**
 * TODO:
 *
 */
use datadog_trace_protobuf::pb;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Bucket {
    pub hits: u32,
}

pub struct StatsConcentrator {
    config: Arc<Config>,
    resource: String,
    buckets: HashMap<u64, Bucket>,
}

const BUCKET_DURATION_NS: u64 = 10 * S_TO_NS_U64;
// TODO: comment
const NO_FLUSH_BUCKET_COUNT: u64 = 2;

impl StatsConcentrator {
    #[must_use]
    pub fn new(config: Arc<Config>, tags_provider: Arc<TagProvider>) -> Self {
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or(String::from("aws.lambda"));
        Self {
            buckets: HashMap::new(),
            config,
            resource,
        }
    }

    pub fn add(&mut self, stats_event: StatsEvent) {
        let bucket_timestamp = Self::get_bucket_timestamp(stats_event.time);
        let bucket = self
            .buckets
            .entry(bucket_timestamp)
            .or_insert(Bucket { hits: 0 });
        bucket.hits += 1;
    }

    fn get_bucket_timestamp(timestamp: u64) -> u64 {
        timestamp - timestamp % BUCKET_DURATION_NS
    }

    #[must_use]
    pub fn get_stats(&mut self, force_flush: bool) -> Vec<pb::ClientStatsPayload> {
        let current_timestamp: u64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Failed to get current timestamp")
            .as_nanos()
            .try_into()
            .expect("Failed to convert timestamp to u64");
        let mut stats = Vec::new();
        let mut to_remove = Vec::new();

        for (&timestamp, bucket) in &self.buckets {
            if force_flush || Self::should_flush_bucket(current_timestamp, timestamp) {
                stats.push(self.construct_stats_payload(timestamp, bucket));
                to_remove.push(timestamp);
            }
        }

        for timestamp in to_remove {
            self.buckets.remove(&timestamp);
        }

        stats
    }

    fn should_flush_bucket(current_timestamp: u64, bucket_timestamp: u64) -> bool {
        current_timestamp - bucket_timestamp >= BUCKET_DURATION_NS * NO_FLUSH_BUCKET_COUNT
    }

    fn construct_stats_payload(&self, timestamp: u64, bucket: &Bucket) -> pb::ClientStatsPayload {
        pb::ClientStatsPayload {
            hostname: String::new(),
            env: self.config.env.clone().unwrap_or_default(),
            version: self.config.version.clone().unwrap_or_default(),
            lang: "rust".to_string(),
            tracer_version: String::new(),
            runtime_id: String::new(),
            sequence: 0,
            agent_aggregation: String::new(),
            service: self.config.service.clone().unwrap_or_default(),
            container_id: String::new(),
            tags: vec![],
            git_commit_sha: String::new(),
            image_tag: String::new(),
            stats: vec![pb::ClientStatsBucket {
                start: timestamp,
                duration: 0,
                stats: vec![pb::ClientGroupedStats {
                    service: self.config.service.clone().unwrap_or_default(),
                    name: "aws.lambda".to_string(),
                    resource: self.resource.clone(),
                    http_status_code: 200,
                    r#type: String::new(),
                    db_type: String::new(),
                    hits: bucket.hits.into(),
                    errors: 0,
                    duration: 0,
                    ok_summary: vec![],
                    error_summary: vec![],
                    synthetics: false,
                    top_level_hits: 0,
                    span_kind: String::new(),
                    peer_tags: vec![],
                    is_trace_root: 1,
                }],
                agent_time_shift: 0,
            }],
        }
    }
}
