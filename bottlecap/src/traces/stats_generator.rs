use crate::traces::stats_concentrator_service::StatsConcentratorHandle;
use libdd_trace_utils::tracer_payload::TracerPayloadCollection;
use tracing::error;

use crate::traces::stats_concentrator_service::StatsError;

pub struct StatsGenerator {
    stats_concentrator: StatsConcentratorHandle,
}

#[derive(Debug, thiserror::Error)]
pub enum StatsGeneratorError {
    #[error("Error sending trace stats to the stats concentrator: {0}")]
    ConcentratorCommandError(StatsError),
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
                // Set tracer metadata
                if let Err(err) = self.stats_concentrator.set_tracer_metadata(trace) {
                    error!("Failed to set tracer metadata: {err}");
                    return Err(StatsGeneratorError::ConcentratorCommandError(err));
                }

                // Generate stats for each span in the trace
                for chunk in &trace.chunks {
                    for span in &chunk.spans {
                        if let Err(err) = self.stats_concentrator.add(span) {
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
