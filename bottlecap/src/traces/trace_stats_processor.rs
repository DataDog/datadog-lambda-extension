use crate::traces::stats_concentrator::{AggregationKey, Stats, StatsEvent};
use crate::traces::stats_concentrator_service::StatsConcentratorHandle;
use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
use tracing::error;

pub struct SendingTraceStatsProcessor {
    stats_concentrator: StatsConcentratorHandle,
}

// Extracts information from traces related to stats and sends it to the stats concentrator
impl SendingTraceStatsProcessor {
    #[must_use]
    pub fn new(stats_concentrator: StatsConcentratorHandle) -> Self {
        Self { stats_concentrator }
    }

    pub fn send(&self, traces: &TracerPayloadCollection) {
        match traces {
            TracerPayloadCollection::V07(traces) => {
                for trace in traces {
                    for chunk in &trace.chunks {
                        for span in &chunk.spans {
                            let stats = StatsEvent {
                                time: span.start.try_into().unwrap_or_default(),
                                aggregation_key: AggregationKey {},
                                stats: Stats {},
                            };
                            if let Err(err) = self.stats_concentrator.add(stats) {
                                error!("Failed to send trace stats: {err}");
                            }
                        }
                    }
                }
            }
            _ => {
                error!("Unsupported trace payload version. Failed to send trace stats.");
            }
        }
    }
}
