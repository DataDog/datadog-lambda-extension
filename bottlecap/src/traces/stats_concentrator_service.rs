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

/// Span kinds eligible for stats computation, matching the Go agent's default
/// `ComputeStatsBySpanKind: true` behavior.
/// Reference: datadog-agent/pkg/trace/stats/span_concentrator.go (KindsComputed)
///
/// TODO: Move to datadog-agent-config in serverless-components so both bottlecap and
/// serverless-compat can share this list.
const STATS_ELIGIBLE_SPAN_KINDS: [&str; 4] = ["client", "consumer", "producer", "server"];

/// Default peer tag keys for stats aggregation, matching the Go agent's `basePeerTags`
/// derived from pkg/trace/semantics/mappings.json via the 16 peer tag concepts.
/// Reference: datadog-agent/pkg/trace/config/peer_tags.go (peerTagConcepts + basePeerTags)
///
/// TODO: Move to datadog-agent-config in serverless-components so both bottlecap and
/// serverless-compat can share this list.
const DEFAULT_PEER_TAG_KEYS: [&str; 43] = [
    "_dd.base_service",
    "active_record.db.vendor",
    "amqp.destination",
    "amqp.exchange",
    "amqp.queue",
    "aws.queue.name",
    "aws.s3.bucket",
    "bucketname",
    "cassandra.keyspace",
    "db.cassandra.contact.points",
    "db.couchbase.seed.nodes",
    "db.hostname",
    "db.instance",
    "db.name",
    "db.namespace",
    "db.system",
    "db.type",
    "dns.hostname",
    "grpc.host",
    "hostname",
    "http.host",
    "http.server_name",
    "messaging.destination",
    "messaging.destination.name",
    "messaging.kafka.bootstrap.servers",
    "messaging.rabbitmq.exchange",
    "messaging.system",
    "mongodb.db",
    "msmq.queue.path",
    "net.peer.name",
    "network.destination.ip",
    "network.destination.name",
    "out.host",
    "peer.hostname",
    "peer.service",
    "queuename",
    "rpc.service",
    "rpc.system",
    "sequel.db.vendor",
    "server.address",
    "streamname",
    "tablename",
    "topicname",
];

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
        let concentrator = SpanConcentrator::new(
            Duration::from_nanos(BUCKET_DURATION_NS),
            SystemTime::now(),
            STATS_ELIGIBLE_SPAN_KINDS.iter().map(|s| s.to_string()).collect(),
            DEFAULT_PEER_TAG_KEYS.iter().map(|s| s.to_string()).collect(),
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
                service: self
                    .config
                    .service
                    .clone()
                    .unwrap_or_default()
                    .to_lowercase(),
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Create a pb::Span with the given meta tags and metrics.
    /// The span is non-root (parent_id=1) and not measured, so it will only be
    /// eligible for stats if span_kinds_stats_computed includes its span.kind.
    fn create_span_kind_span(
        span_kind: &str,
        meta: Vec<(&str, &str)>,
    ) -> pb::Span {
        let now_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let mut meta_map: HashMap<String, String> = meta
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        meta_map.insert("span.kind".to_string(), span_kind.to_string());
        pb::Span {
            service: "test-service".to_string(),
            name: "test-op".to_string(),
            resource: "test-resource".to_string(),
            trace_id: 1,
            span_id: 2,
            parent_id: 1, // non-root
            start: now_ns,
            duration: 100,
            error: 0,
            r#type: "web".to_string(),
            meta: meta_map,
            metrics: HashMap::new(), // no _top_level, no _dd.measured
            meta_struct: HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        }
    }

    /// A non-root, non-measured span with span.kind="client" should produce stats
    /// when ComputeStatsBySpanKind is enabled (i.e. span_kinds_stats_computed is
    /// populated). With the current empty vec, this span is silently dropped.
    #[tokio::test]
    async fn test_span_kind_stats_computed() {
        let config = Arc::new(Config::default());
        let (service, handle) = StatsConcentratorService::new(config);
        tokio::spawn(service.run());

        let span = create_span_kind_span("client", vec![]);
        handle.add(&span).unwrap();

        let result = handle.flush(true).await.unwrap();

        assert!(
            result.is_some(),
            "Expected stats for a client span, but got None. \
             span.kind-based eligibility is not working."
        );
        let payload = result.unwrap();
        let all_stats: Vec<_> = payload.stats.iter().flat_map(|b| &b.stats).collect();
        assert!(
            !all_stats.is_empty(),
            "Expected at least one grouped stats entry for the client span."
        );
        let client_stats: Vec<_> = all_stats
            .iter()
            .filter(|s| s.span_kind == "client")
            .collect();
        assert!(
            !client_stats.is_empty(),
            "Expected a stats entry with span_kind='client'."
        );
    }

    /// A client span with peer tag meta keys (db.instance, db.system) should produce
    /// stats with non-empty peer_tags when peer_tag_keys is configured. With the
    /// current empty vec, peer_tags in the output will always be empty.
    #[tokio::test]
    async fn test_peer_tags_populated() {
        let config = Arc::new(Config::default());
        let (service, handle) = StatsConcentratorService::new(config);
        tokio::spawn(service.run());

        let span = create_span_kind_span(
            "client",
            vec![("db.instance", "i-1234"), ("db.system", "postgres")],
        );
        handle.add(&span).unwrap();

        let result = handle.flush(true).await.unwrap();

        assert!(
            result.is_some(),
            "Expected stats for a client span with peer tags, but got None. \
             span.kind-based eligibility is not working."
        );
        let payload = result.unwrap();
        let all_stats: Vec<_> = payload.stats.iter().flat_map(|b| &b.stats).collect();
        let stats_with_peer_tags: Vec<_> = all_stats
            .iter()
            .filter(|s| !s.peer_tags.is_empty())
            .collect();
        assert!(
            !stats_with_peer_tags.is_empty(),
            "Expected at least one stats entry with non-empty peer_tags, \
             but all entries have empty peer_tags."
        );
        let peer_tags = &stats_with_peer_tags[0].peer_tags;
        assert!(
            peer_tags.iter().any(|t| t.starts_with("db.instance:")),
            "Expected peer_tags to contain db.instance, got: {peer_tags:?}"
        );
        assert!(
            peer_tags.iter().any(|t| t.starts_with("db.system:")),
            "Expected peer_tags to contain db.system, got: {peer_tags:?}"
        );
    }
}
