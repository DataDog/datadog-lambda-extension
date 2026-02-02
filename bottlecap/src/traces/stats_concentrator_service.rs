use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use libdd_trace_protobuf::pb;
use libdd_trace_protobuf::pb::{ClientStatsPayload, TracerPayload};
use libdd_trace_stats::span_concentrator::SpanConcentrator;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
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

#[derive(Clone, Debug, Default)]
pub struct TracerMetadata {
    // e.g. "python"
    pub language: String,
    // e.g. "3.11.0"
    pub tracer_version: String,
    // e.g. "f45568ad09d5480b99087d86ebda26e6"
    pub runtime_id: String,
    pub container_id: String,
}

pub enum ConcentratorCommand {
    SetTracerMetadata(TracerMetadata),
    // Use a box to reduce the size of the command enum
    Add(Box<pb::Span>),
    Flush(bool, oneshot::Sender<Option<ClientStatsPayload>>),
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
                container_id: trace.container_id.clone(),
            };
            self.tx
                .send(ConcentratorCommand::SetTracerMetadata(tracer_metadata))
                .map_err(StatsError::SendError)?;
        }
        Ok(())
    }

    pub fn add(&self, span: &pb::Span) -> Result<(), StatsError> {
        self.tx
            .send(ConcentratorCommand::Add(Box::new(span.clone())))
            .map_err(StatsError::SendError)?;
        Ok(())
    }

    pub async fn flush(&self, force_flush: bool) -> Result<Option<ClientStatsPayload>, StatsError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ConcentratorCommand::Flush(force_flush, response_tx))
            .map_err(StatsError::SendError)?;
        response_rx.await.map_err(StatsError::RecvError)
    }
}

pub struct StatsConcentratorService {
    concentrator: SpanConcentrator,
    rx: mpsc::UnboundedReceiver<ConcentratorCommand>,
    tracer_metadata: TracerMetadata,
    config: Arc<Config>,
}

// A service that handles add() and flush() requests in the same queue,
// to avoid using mutex, which may cause lock contention.
impl StatsConcentratorService {
    #[must_use]
    pub fn new(config: Arc<Config>) -> (Self, StatsConcentratorHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = StatsConcentratorHandle::new(tx);
        // TODO: set span_kinds_stats_computed and peer_tag_keys
        let concentrator = SpanConcentrator::new(
            Duration::from_nanos(BUCKET_DURATION_NS),
            SystemTime::now(),
            vec![],
            vec![],
        );
        let service: StatsConcentratorService = Self {
            concentrator,
            rx,
            // To be set when the first trace is received
            tracer_metadata: TracerMetadata::default(),
            config,
        };
        (service, handle)
    }

    pub async fn run(mut self) {
        while let Some(command) = self.rx.recv().await {
            match command {
                ConcentratorCommand::SetTracerMetadata(tracer_metadata) => {
                    self.tracer_metadata = tracer_metadata;
                }
                ConcentratorCommand::Add(span) => self.concentrator.add_span(&*span),
                ConcentratorCommand::Flush(force_flush, response_tx) => {
                    self.handle_flush(force_flush, response_tx);
                }
            }
        }
    }

    fn handle_flush(
        &mut self,
        force_flush: bool,
        response_tx: oneshot::Sender<Option<ClientStatsPayload>>,
    ) {
        let stats_buckets = self.concentrator.flush(SystemTime::now(), force_flush);
        let stats = if stats_buckets.is_empty() {
            None
        } else {
            Some(ClientStatsPayload {
                // Do not set hostname so the trace stats backend can aggregate stats properly
                hostname: String::new(),
                env: self.config.env.clone().unwrap_or("unknown-env".to_string()),
                // Version is not in the trace payload. Need to read it from config.
                version: self.config.version.clone().unwrap_or_default(),
                lang: self.tracer_metadata.language.clone(),
                tracer_version: self.tracer_metadata.tracer_version.clone(),
                runtime_id: self.tracer_metadata.runtime_id.clone(),
                // Not supported yet
                sequence: 0,
                // Not supported yet
                agent_aggregation: String::new(),
                service: self.config.service.clone().unwrap_or_default().to_lowercase(),
                container_id: self.tracer_metadata.container_id.clone(),
                // Not supported yet
                tags: vec![],
                // Not supported yet
                git_commit_sha: String::new(),
                // Not supported yet
                image_tag: String::new(),
                stats: stats_buckets,
                // Not supported yet
                process_tags: String::new(),
                // Not supported yet
                process_tags_hash: 0,
            })
        };
        let response = response_tx.send(stats);
        if let Err(e) = response {
            error!("Failed to return trace stats: {e:?}");
        }
    }
}
