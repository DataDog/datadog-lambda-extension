use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::traces::stats_concentrator::StatsConcentrator;
use crate::traces::stats_concentrator::StatsEvent;
use crate::traces::stats_concentrator::TracerMetadata;
use datadog_trace_protobuf::pb;
use datadog_trace_protobuf::pb::TracerPayload;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
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

pub struct StatsConcentratorHandle {
    tx: mpsc::UnboundedSender<ConcentratorCommand>,
    is_tracer_metadata_set: AtomicBool,
}

impl Clone for StatsConcentratorHandle {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            // Cloning this may cause trace metadata to be set multiple times,
            // but it's okay because it's the same for all traces and we don't need to be perfect on dedup.
            is_tracer_metadata_set: AtomicBool::new(
                self.is_tracer_metadata_set.load(Ordering::Acquire),
            ),
        }
    }
}

impl StatsConcentratorHandle {
    #[must_use]
    pub fn new(tx: mpsc::UnboundedSender<ConcentratorCommand>) -> Self {
        Self {
            tx,
            is_tracer_metadata_set: AtomicBool::new(false),
        }
    }

    pub fn set_tracer_metadata(&self, trace: &TracerPayload) -> Result<(), StatsError> {
        // Set tracer metadata only once for the first trace because
        // it is the same for all traces.
        if !self.is_tracer_metadata_set.load(Ordering::Acquire) {
            self.is_tracer_metadata_set.store(true, Ordering::Release);
            let tracer_metadata = TracerMetadata {
                language: trace.language_name.clone(),
                tracer_version: trace.tracer_version.clone(),
                runtime_id: trace.runtime_id.clone(),
            };
            self.tx
                .send(ConcentratorCommand::SetTracerMetadata(tracer_metadata))
                .map_err(StatsError::SendError)?;
        }
        Ok(())
    }

    pub fn add(&self, stats_event: StatsEvent) -> Result<(), StatsError> {
        self.tx
            .send(ConcentratorCommand::Add(stats_event))
            .map_err(StatsError::SendError)?;
        Ok(())
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
        let handle = StatsConcentratorHandle::new(tx);
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
