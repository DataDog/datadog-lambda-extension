use tokio::sync::{mpsc, oneshot};

use datadog_trace_stats::span_concentrator::SpanConcentrator;
use datadog_trace_protobuf::pb;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, Duration};
use tracing::error;

const S_TO_NS: u64 = 1_000_000_000;
const BUCKET_DURATION_NS: u64 = 10 * S_TO_NS; // 10 seconds

#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error("Failed to send command to concentrator: {0}")]
    SendError(mpsc::error::SendError<ConcentratorCommand>),
    #[error("Failed to receive response from concentrator: {0}")]
    RecvError(oneshot::error::RecvError),
}

pub enum ConcentratorCommand {
    // SetTracerMetadata(TracerMetadata),
    Add(Box<pb::Span>),
    Flush(bool, oneshot::Sender<Vec<pb::ClientStatsBucket>>),
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

    pub fn add(&self, span: &pb::Span) -> Result<(), StatsError> {
        self.tx
            .send(ConcentratorCommand::Add(Box::new(span.clone())))
            .map_err(StatsError::SendError)?;
        Ok(())
    }

    pub async fn flush(
        &self,
        force_flush: bool,
    ) -> Result<Vec<pb::ClientStatsBucket>, StatsError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ConcentratorCommand::Flush(force_flush, response_tx))
            .map_err(StatsError::SendError)?;
        let stats = response_rx.await.map_err(StatsError::RecvError)?;
        Ok(stats)
    }
}

pub struct StatsConcentratorService {
    concentrator: SpanConcentrator,
    rx: mpsc::UnboundedReceiver<ConcentratorCommand>,
}

// A service that handles add() and flush() requests in the same queue,
// to avoid using mutex, which may cause lock contention.
impl StatsConcentratorService {
    #[must_use]
    pub fn new() -> (Self, StatsConcentratorHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = StatsConcentratorHandle::new(tx);
        // TODO: set span_kinds_stats_computed and peer_tag_keys
        let concentrator = SpanConcentrator::new(Duration::from_nanos(BUCKET_DURATION_NS), SystemTime::now(), vec![], vec![]);
        let service: StatsConcentratorService = Self { concentrator, rx };
        (service, handle)
    }

    pub async fn run(mut self) {
        while let Some(command) = self.rx.recv().await {
            match command {
                ConcentratorCommand::Add(span) => self.concentrator.add_span(&*span),
                ConcentratorCommand::Flush(force_flush, response_tx) => {
                    let stats = self.concentrator.flush(SystemTime::now(), force_flush);
                    if let Err(e) = response_tx.send(stats) {
                        error!("Failed to return trace stats: {e:?}");
                    }
                }
            }
        }
    }
}
