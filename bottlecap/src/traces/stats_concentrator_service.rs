use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::traces::stats_concentrator::StatsConcentrator;
use crate::traces::stats_concentrator::StatsEvent;
use datadog_trace_protobuf::pb;
use std::sync::Arc;
use tracing::error;

#[derive(Debug)]
pub enum StatsError {
    SendError(mpsc::error::SendError<ConcentratorCommand>),
    RecvError(oneshot::error::RecvError),
}

impl From<mpsc::error::SendError<ConcentratorCommand>> for StatsError {
    fn from(err: mpsc::error::SendError<ConcentratorCommand>) -> Self {
        StatsError::SendError(err)
    }
}

impl From<oneshot::error::RecvError> for StatsError {
    fn from(err: oneshot::error::RecvError) -> Self {
        StatsError::RecvError(err)
    }
}

pub enum ConcentratorCommand {
    Add(StatsEvent),
    GetStats(bool, oneshot::Sender<Vec<pb::ClientStatsPayload>>),
}

#[derive(Clone)]
pub struct StatsConcentratorHandle {
    tx: mpsc::UnboundedSender<ConcentratorCommand>,
}

impl StatsConcentratorHandle {
    pub fn add(
        &self,
        stats_event: StatsEvent,
    ) -> Result<(), mpsc::error::SendError<ConcentratorCommand>> {
        self.tx.send(ConcentratorCommand::Add(stats_event))
    }

    pub async fn get_stats(
        &self,
        force_flush: bool,
    ) -> Result<Vec<pb::ClientStatsPayload>, StatsError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ConcentratorCommand::GetStats(force_flush, response_tx))?;
        let stats = response_rx.await?;
        Ok(stats)
    }
}

pub struct StatsConcentratorService {
    concentrator: StatsConcentrator,
    rx: mpsc::UnboundedReceiver<ConcentratorCommand>,
}

// A service that handles add() and get_stats() requests in the same queue,
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
                ConcentratorCommand::Add(stats_event) => self.concentrator.add(stats_event),
                ConcentratorCommand::GetStats(force_flush, response_tx) => {
                    let stats = self.concentrator.get_stats(force_flush);
                    if let Err(e) = response_tx.send(stats) {
                        error!("Failed to return trace stats: {e:?}");
                    }
                }
            }
        }
    }
}
