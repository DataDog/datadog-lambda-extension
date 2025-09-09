/**
 * TODO:
 * 
 */

use datadog_trace_protobuf::pb;
use tracing::debug;

#[derive(Default)]
pub struct StatsConcentrator {
    pub storage: Vec<pb::ClientStatsPayload>,
}

impl StatsConcentrator {
    #[must_use]
    pub fn new() -> Self {
        Self { storage: Vec::new() }
    }

    pub fn add(&mut self, stats: pb::ClientStatsPayload) {
        debug!("StatsConcentrator | adding stats payload to concentrator: {stats:?}");
        self.storage.push(stats);
    }

    #[must_use]
    pub fn get_batch(&mut self) -> Vec<pb::ClientStatsPayload> {
        let ret = self.storage.clone();
        self.storage.clear();
        ret
    }
}