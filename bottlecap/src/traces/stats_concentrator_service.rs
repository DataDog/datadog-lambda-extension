use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::traces::stats_concentrator::StatsConcentrator;
use crate::traces::stats_concentrator::StatsEvent;
use crate::traces::stats_concentrator::TracerMetadata;
use datadog_trace_protobuf::pb;
use std::sync::Arc;
use tracing::error;

#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error("Failed to send command to concentrator: {0}")]
    SendError(mpsc::error::SendError<ConcentratorCommand>),
    #[error("Failed to receive response from concentrator: {0}")]
    RecvError(oneshot::error::RecvError),
}

pub enum ConcentratorCommand {
    SetTracerMetadata(TracerMetadata),
    Add(StatsEvent),
    Flush(bool, oneshot::Sender<Vec<pb::ClientStatsPayload>>),
}

#[derive(Clone)]
pub struct StatsConcentratorHandle {
    tx: mpsc::UnboundedSender<ConcentratorCommand>,
}

impl StatsConcentratorHandle {
    pub fn set_tracer_metadata(
        &self,
        tracer_metadata: &TracerMetadata,
    ) -> Result<(), mpsc::error::SendError<ConcentratorCommand>> {
        self.tx.send(ConcentratorCommand::SetTracerMetadata(
            tracer_metadata.clone(),
        ))
    }

    pub fn add(
        &self,
        stats_event: StatsEvent,
    ) -> Result<(), mpsc::error::SendError<ConcentratorCommand>> {
        self.tx.send(ConcentratorCommand::Add(stats_event))
    }

    pub async fn flush(
        &self,
        force_flush: bool,
    ) -> Result<Vec<pb::ClientStatsPayload>, StatsError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ConcentratorCommand::Flush(force_flush, response_tx))
            .map_err(StatsError::SendError)?;
        let stats = response_rx.await.map_err(StatsError::RecvError)?;
        Ok(stats)
    }
}

pub struct StatsConcentratorService {
    concentrator: StatsConcentrator,
    rx: mpsc::UnboundedReceiver<ConcentratorCommand>,
}

// A service that handles add() and flush() requests in the same queue,
// to avoid using mutex, which may cause lock contention.
impl StatsConcentratorService {
    #[must_use]
    pub fn new(config: Arc<Config>) -> (Self, StatsConcentratorHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = StatsConcentratorHandle { tx };
        let concentrator = StatsConcentrator::new(config);
        let service: StatsConcentratorService = Self { concentrator, rx };
        (service, handle)
    }

    pub async fn run(mut self) {
        while let Some(command) = self.rx.recv().await {
            match command {
                ConcentratorCommand::SetTracerMetadata(tracer_metadata) => {
                    self.concentrator.set_tracer_metadata(&tracer_metadata);
                }
                ConcentratorCommand::Add(stats_event) => self.concentrator.add(stats_event),
                ConcentratorCommand::Flush(force_flush, response_tx) => {
                    let stats = self.concentrator.flush(force_flush);
                    if let Err(e) = response_tx.send(stats) {
                        error!("Failed to return trace stats: {e:?}");
                    }
                }
            }
        }
    }
}
