use crate::config::Config;
use datadog_trace_protobuf::pb;
use std::{collections::HashMap, sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use tracing::error;

// Event sent to the stats concentrator
#[derive(Clone)]
pub struct StatsEvent {
    pub time: u64,
    pub aggregation_key: AggregationKey,
    pub stats: Stats,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AggregationKey {
    pub env: String,
    pub service: String,
    // e.g. "aws.lambda.load", "aws.lambda.import"
    pub name: String,
    // e.g. "my-lambda-function-name", "datadog_lambda.handler", "urllib.request"
    pub resource: String,
    // e.g. "aws.lambda.load", "aws.lambda.import"
    pub r#type: String,
}

// Aggregated stats for a time interval across all the aggregation keys.
#[derive(Default)]
struct Bucket {
    data: HashMap<AggregationKey, Stats>,
}

#[derive(Clone, Debug, Default, Copy)]
pub struct Stats {
    pub hits: i32,
    pub duration: i64, // in nanoseconds
    pub error: i32,
    pub top_level_hits: f64,
}

pub struct StatsConcentrator {
    config: Arc<Config>,
    buckets: HashMap<u64, Bucket>,
}


// The number of latest buckets to not flush when force_flush is false.
// For example, if we have buckets with timestamps 10, 20, 40, the current timestamp is 45,
// and NO_FLUSH_BUCKET_COUNT is 3, then we will flush bucket 10 but not bucket 20 or 40.
// Note that the bucket 30 is included in the 3 latest buckets even if it has no data.
// This is to reduce the chance of flushing stats that are still being collected to save some cost.
const NO_FLUSH_BUCKET_COUNT: u64 = 2;

const S_TO_NS_U64: u64 = 1_000_000_000;

// The duration of a bucket in nanoseconds.
const BUCKET_DURATION_NS: u64 = 10 * S_TO_NS_U64; // 10 seconds


// Aggregates stats into buckets, which are then pulled by the stats aggregator.
impl StatsConcentrator {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self { config, buckets: HashMap::new() }
    }

    pub fn add(&mut self, stats_event: StatsEvent) {
        let bucket_timestamp = Self::get_bucket_timestamp(stats_event.time);
        let bucket = self.buckets.entry(bucket_timestamp).or_default();

        let stats = bucket.data.entry(stats_event.aggregation_key).or_default();

        stats.hits += stats_event.stats.hits;
        stats.error += stats_event.stats.error;
        stats.duration += stats_event.stats.duration;
        stats.top_level_hits += stats_event.stats.top_level_hits;
    }

    fn get_bucket_timestamp(timestamp: u64) -> u64 {
        timestamp - timestamp % BUCKET_DURATION_NS
    }

    // force_flush: If true, flush all stats. If false, flush stats except for the few latest
    // buckets, which may still be getting data.
    #[must_use]
    pub fn flush(&mut self, force_flush: bool) -> Vec<pb::ClientStatsPayload> {
        let current_timestamp: u64 = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => {
                if let Ok(ts) = duration.as_nanos().try_into() {
                    ts
                } else {
                    error!("Timestamp overflow, skipping stats flush");
                    return Vec::new();
                }
            }
            Err(e) => {
                error!("Failed to get current timestamp: {e}, skipping stats flush");
                return Vec::new();
            }
        };

        let mut ret = Vec::new();
        self.buckets.retain(|&timestamp, bucket| {
            if force_flush || Self::should_flush_bucket(current_timestamp, timestamp) {
                // Flush and remove this bucket
                for (aggregation_key, stats) in &bucket.data {
                    ret.push(Self::construct_stats_payload(
                        &self.config,
                        timestamp,
                        aggregation_key,
                        *stats,
                    ));
                }
                false
            } else {
                // Keep this bucket
                true
            }
        });

        ret
    }

    // Whether a bucket should be flushed based on the current timestamp and the bucket timestamp.
    fn should_flush_bucket(current_timestamp: u64, bucket_timestamp: u64) -> bool {
        current_timestamp - bucket_timestamp >= BUCKET_DURATION_NS * NO_FLUSH_BUCKET_COUNT
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    fn construct_stats_payload(
        config: &Config,
        timestamp: u64,
        aggregation_key: &AggregationKey,
        stats: Stats,
    ) -> pb::ClientStatsPayload {
        pb::ClientStatsPayload {
            hostname: String::new(),
            env: aggregation_key.env.clone(),
            version: config.version.clone().unwrap_or_default(),
            lang: "rust".to_string(),
            tracer_version: String::new(),
            runtime_id: String::new(),
            sequence: 0,
            agent_aggregation: String::new(),
            service: aggregation_key.service.clone(),
            container_id: String::new(),
            tags: vec![],
            git_commit_sha: String::new(),
            image_tag: String::new(),
            stats: vec![pb::ClientStatsBucket {
                start: timestamp,
                duration: BUCKET_DURATION_NS,
                stats: vec![pb::ClientGroupedStats {
                    service: aggregation_key.service.clone(),
                    name: aggregation_key.name.clone(),
                    resource: aggregation_key.resource.clone(),
                    http_status_code: 0,
                    r#type: aggregation_key.r#type.clone(),
                    db_type: String::new(),
                    hits: stats.hits.try_into().unwrap_or_default(),
                    errors: stats.error.try_into().unwrap_or_default(),
                    duration: stats.duration.try_into().unwrap_or_default(),
                    ok_summary: vec![],
                    error_summary: vec![],
                    synthetics: false,
                    top_level_hits: stats.top_level_hits.round() as u64,
                    span_kind: String::new(),
                    peer_tags: vec![],
                    is_trace_root: 1,
                }],
                agent_time_shift: 0,
            }],
        }
    }
}
