use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::error::SendError;
use tracing::debug;

use super::stats_agent::StatsEvent;
use super::stats_concentrator::AggregationKey;
use super::stats_concentrator::Stats;

use datadog_trace_protobuf::pb;

pub struct SendingTraceStatsProcessor {
    stats_tx: Sender<StatsEvent>,
}

impl SendingTraceStatsProcessor {
    #[must_use]
    pub fn new(stats_tx: Sender<StatsEvent>) -> Self {
        Self { stats_tx }
    }

    pub async fn send(
        &self,
        traces: &[Vec<pb::Span>],
    ) -> Result<(), SendError<StatsEvent>> {
        debug!("Sending stats to the stats concentrator");
        for trace in traces {
            for span in trace {
                let stats = StatsEvent {
                    time: span.start.try_into().unwrap_or_default(),
                    aggregation_key: AggregationKey {
                        name: span.name.clone(),
                        resource: span.resource.clone(),
                    },
                    stats: Stats {
                        // TODO: handle error == 1
                        hits: 1,
                    },
                };
                debug!("Sending single stats to the stats concentrator.");
                self.stats_tx.send(stats).await?;
            }
        }
        Ok(())
    }
}