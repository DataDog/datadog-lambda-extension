use crate::traces::stats_concentrator::{AggregationKey, Stats, StatsEvent};
use crate::traces::stats_concentrator_service::StatsConcentratorHandle;
use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
use tracing::{debug, error};

use tokio::sync::mpsc::error::SendError;

use crate::traces::stats_concentrator_service::ConcentratorCommand;

pub struct StatsGenerator {
    stats_concentrator: StatsConcentratorHandle,
}

#[derive(Debug, thiserror::Error)]
pub enum StatsGeneratorError {
    #[error("Error sending trace stats to the stats concentrator: {0}")]
    ConcentratorCommandError(SendError<ConcentratorCommand>),
    #[error("Unsupported trace payload version. Failed to send trace stats.")]
    TracePayloadVersionError,
}

// Extracts information from traces related to stats and sends it to the stats concentrator
impl StatsGenerator {
    #[must_use]
    pub fn new(stats_concentrator: StatsConcentratorHandle) -> Self {
        Self { stats_concentrator }
    }

    pub fn send(&self, traces: &TracerPayloadCollection) -> Result<(), StatsGeneratorError> {
        if let TracerPayloadCollection::V07(traces) = traces {
            for trace in traces {
                for chunk in &trace.chunks {
                    for span in &chunk.spans {
                        let stats = StatsEvent {
                            time: span.start.try_into().unwrap_or_default(),
                            aggregation_key: AggregationKey {
                                env: span.meta.get("env").cloned().unwrap_or_default(),
                                service: span.service.clone(),
                                name: span.name.clone(),
                                resource: span.resource.clone(),
                                r#type: span.r#type.clone(),
                            },
                            stats: Stats {
                                hits: 1,
                                error: span.error,
                                duration: span.duration,
                                top_level_hits: span
                                    .metrics
                                    .get("_dd.top_level")
                                    .map_or(0.0, |v| *v),
                            },
                        };
                        if let Err(err) = self.stats_concentrator.add(stats) {
                            error!("Failed to send trace stats: {err}");
                            return Err(StatsGeneratorError::ConcentratorCommandError(err));
                        }
                    }
                }
            }
            Ok(())
        } else {
            error!("Unsupported trace payload version. Failed to send trace stats.");
            Err(StatsGeneratorError::TracePayloadVersionError)
        }
    }
}
