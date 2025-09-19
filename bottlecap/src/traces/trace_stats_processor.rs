use tokio::sync::mpsc::error::SendError;

use tracing::debug;

use crate::traces::stats_concentrator::{AggregationKey, Stats, StatsEvent};
use crate::traces::stats_concentrator_service::{ConcentratorCommand, StatsConcentratorHandle};
use datadog_trace_protobuf::pb;

pub struct SendingTraceStatsProcessor {
    stats_concentrator: StatsConcentratorHandle,
}

// Extracts information from traces related to stats and sends it to the stats concentrator
impl SendingTraceStatsProcessor {
    #[must_use]
    pub fn new(stats_concentrator: StatsConcentratorHandle) -> Self {
        Self { stats_concentrator }
    }

    pub fn send(&self, traces: &[Vec<pb::Span>]) -> Result<(), SendError<ConcentratorCommand>> {
        debug!("Sending trace stats to the concentrator");
        for trace in traces {
            for span in trace {
                let stats = StatsEvent {
                    time: span.start.try_into().unwrap_or_default(),
                    aggregation_key: AggregationKey {},
                    stats: Stats {},
                };
                self.stats_concentrator.add(stats)?;
            }
        }
        Ok(())
    }
}
