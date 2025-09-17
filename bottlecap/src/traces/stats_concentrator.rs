use crate::config::Config;
use crate::traces::stats_agent::StatsEvent;
use datadog_trace_protobuf::pb;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Copy)]
pub struct AggregationKey {}

#[derive(Clone, Debug, Default, Copy)]
pub struct Stats {}

pub struct StatsConcentrator {
    _config: Arc<Config>,
}

// Aggregates stats into buckets, which are then pulled by the stats aggregator.
impl StatsConcentrator {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self { _config: config }
    }

    pub fn add(&mut self, _stats_event: StatsEvent) {}

    // force_flush: If true, flush all stats. If false, flush stats except for the few latest
    // buckets, which may still be getting data.
    #[must_use]
    pub fn get_stats(&mut self, _force_flush: bool) -> Vec<pb::ClientStatsPayload> {
        vec![]
    }
}
