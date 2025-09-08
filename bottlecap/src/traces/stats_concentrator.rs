/**
 * TODO:
 * 
 */

use datadog_trace_protobuf::pb;

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
        self.storage.push(stats);
    }

    #[must_use]
    pub fn get_batch(&mut self) -> Vec<pb::ClientStatsPayload> {
        let ret = self.storage.clone();
        self.storage.clear();
        ret
    }
}