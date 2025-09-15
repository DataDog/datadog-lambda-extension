use crate::config::Config;
use crate::lifecycle::invocation::processor::S_TO_NS_U64;
use crate::traces::stats_agent::StatsEvent;
use datadog_trace_protobuf::pb;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AggregationKey {
    // TODO: add more fields
    pub name: String,
    pub resource: String,
}

// Aggregated stats for a time interval across all the aggregation keys.
#[derive(Default)]
struct Bucket {
    data: HashMap<AggregationKey, Stats>,
}

#[derive(Clone, Debug, Default, Copy)]
pub struct Stats {
    // TODO: add more fields
    pub hits: i32,
    pub duration: i64, // in nanoseconds
    pub error: i32,
}

pub struct StatsConcentrator {
    config: Arc<Config>,
    buckets: HashMap<u64, Bucket>,
}

// The duration of a bucket in nanoseconds.
const BUCKET_DURATION_NS: u64 = 10 * S_TO_NS_U64; // 10 seconds

// The number of latest buckets to not flush when force_flush is false.
// For example, if we have buckets with timestamps 10, 20, 40, the current timestamp is 45,
// and NO_FLUSH_BUCKET_COUNT is 3, then we will flush bucket 10 but not bucket 20 or 40.
// Note that the bucket 30 is included in the 3 latest buckets even if it has no data.
// This is to reduce the chance of flushing stats that are still being collected to save some cost.
const NO_FLUSH_BUCKET_COUNT: u64 = 2;

// Aggregates stats into buckets, which are then pulled by the stats aggregator.
impl StatsConcentrator {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            buckets: HashMap::new(),
            config,
        }
    }

    pub fn add(&mut self, stats_event: StatsEvent) {
        debug!("Adding stats to the stats concentrator");
        let bucket_timestamp = Self::get_bucket_timestamp(stats_event.time);
        let bucket = self.buckets.entry(bucket_timestamp).or_default();

        let stats = bucket.data.entry(stats_event.aggregation_key).or_default();

        stats.hits += stats_event.stats.hits;
    }

    fn get_bucket_timestamp(timestamp: u64) -> u64 {
        timestamp - timestamp % BUCKET_DURATION_NS
    }

    // force_flush: If true, flush all stats. If false, flush stats except for the few latest
    // buckets, which may still be getting data.
    #[must_use]
    pub fn get_stats(&mut self, force_flush: bool) -> Vec<pb::ClientStatsPayload> {
        let current_timestamp: u64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Failed to get current timestamp")
            .as_nanos()
            .try_into()
            .expect("Failed to convert timestamp to u64");
        let mut ret = Vec::new();
        let mut to_remove = Vec::new();

        for (&timestamp, bucket) in &self.buckets {
            if !force_flush && !Self::should_flush_bucket(current_timestamp, timestamp) {
                continue;
            }

            for (aggregation_key, stats) in &bucket.data {
                ret.push(self.construct_stats_payload(timestamp, aggregation_key, *stats));
            }
            to_remove.push(timestamp);
        }

        for timestamp in to_remove {
            self.buckets.remove(&timestamp);
        }

        ret
    }

    // Whether a bucket should be flushed based on the current timestamp and the bucket timestamp.
    fn should_flush_bucket(current_timestamp: u64, bucket_timestamp: u64) -> bool {
        current_timestamp - bucket_timestamp >= BUCKET_DURATION_NS * NO_FLUSH_BUCKET_COUNT
    }

    fn construct_stats_payload(
        &self,
        timestamp: u64,
        aggregation_key: &AggregationKey,
        stats: Stats,
    ) -> pb::ClientStatsPayload {
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
                duration: BUCKET_DURATION_NS,
                stats: vec![pb::ClientGroupedStats {
                    service: self.config.service.clone().unwrap_or_default(),
                    name: aggregation_key.name.clone(),
                    resource: aggregation_key.resource.clone(),
                    http_status_code: 0,
                    r#type: String::new(),
                    db_type: String::new(),
                    hits: stats.hits.try_into().unwrap_or_default(),
                    errors: stats.error.try_into().unwrap_or_default(),
                    duration: stats.duration.try_into().unwrap_or_default(),
                    ok_summary: vec![],
                    error_summary: vec![],
                    synthetics: false,
                    // TODO: handle top_level_hits
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_flush_bucket_false_when_not_enough_time_passed() {
        let bucket_timestamp = 1_000_000_000;
        let current_timestamp = bucket_timestamp + BUCKET_DURATION_NS * (NO_FLUSH_BUCKET_COUNT - 1);
        assert!(
            !StatsConcentrator::should_flush_bucket(current_timestamp, bucket_timestamp),
            "Should not flush when current_timestamp is less than threshold ahead"
        );
    }

    #[test]
    fn test_should_flush_bucket_true_when_much_later() {
        let bucket_timestamp = 1_000_000_000;
        let current_timestamp = bucket_timestamp + BUCKET_DURATION_NS * (NO_FLUSH_BUCKET_COUNT + 5);
        assert!(
            StatsConcentrator::should_flush_bucket(current_timestamp, bucket_timestamp),
            "Should flush when current_timestamp is much greater than threshold"
        );
    }
}
