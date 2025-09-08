/**
 * TODO:
 * 
 */

use datadog_trace_protobuf::pb;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::traces::stats_aggregator::StatsAggregator;

pub struct StatsConcentrator {
    // pub storage: Vec<pb::ClientStatsPayload>,
    stats_aggregator: Arc<Mutex<StatsAggregator>>,
}

impl StatsConcentrator {
    pub fn new(stats_aggregator: Arc<Mutex<StatsAggregator>>) -> Self {
        Self { stats_aggregator }
    }

    pub async fn add(&mut self, stats: pb::ClientStatsPayload) {
        self.stats_aggregator.lock().await.add(stats);
    }

    // pub fn get_batch(&mut self) -> Vec<pb::ClientStatsPayload> {
    //     let ret = self.storage.clone();
    //     self.storage.clear();
    //     ret
    // }
}