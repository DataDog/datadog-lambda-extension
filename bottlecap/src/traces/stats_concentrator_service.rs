use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use libdd_trace_protobuf::pb;
use libdd_trace_protobuf::pb::{ClientStatsPayload, TracerPayload};
use libdd_trace_stats::span_concentrator::{CardinalityLimitConfig, SpanConcentrator};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};
use tracing::{error, warn};

const S_TO_NS: u64 = 1_000_000_000;
const BUCKET_DURATION_NS: u64 = 10 * S_TO_NS; // 10 seconds

/// Sentinel libdatadog rewrites collapsed aggregation key fields to.
///
/// Hand-copied: upstream declares `pub const TRACER_BLOCKED_VALUE` in
/// `libdd-trace-stats/src/span_concentrator/aggregation.rs`, but `mod aggregation` is private and
/// the constant is not re-exported, so it cannot be imported. A one-line upstream `pub use` would
/// remove this copy.
const TRACER_BLOCKED_VALUE: &str = "tracer_blocked_value";

/// Span kinds eligible for stats computation, matching the Go agent's default
/// `ComputeStatsBySpanKind: true` behavior.
/// Reference: `datadog-agent/pkg/trace/stats/span_concentrator.go` (`KindsComputed`)
///
/// TODO: The source of truth is the Go agent's `KindsComputed`; this list is hand-copied here
/// and in other Rust repos. Refactor so they stay in sync with the Go agent instead of each
/// keeping its own copy.
const STATS_ELIGIBLE_SPAN_KINDS: &[&str] = &["client", "consumer", "producer", "server"];

/// Default peer tag keys for stats aggregation, matching the Go agent's `basePeerTags`
/// derived from pkg/trace/semantics/mappings.json via the 16 peer tag concepts.
/// Reference: `datadog-agent/pkg/trace/config/peer_tags.go` (`peerTagConcepts` + `basePeerTags`)
///
/// TODO: The source of truth is the Go agent's `basePeerTags` (derived from
/// pkg/trace/semantics/mappings.json); this list is hand-copied here and in other Rust repos.
/// Refactor so they stay in sync with the Go agent instead of each keeping its own copy.
const DEFAULT_PEER_TAG_KEYS: &[&str] = &[
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

/// Bitset of aggregation key fields that libdatadog collapsed into [`TRACER_BLOCKED_VALUE`]
/// because they exceeded their per-bucket cardinality limit.
///
/// A bitset rather than four `bool`s both to satisfy `clippy::struct_excessive_bools` and to
/// mirror libdatadog's own `CollapsedFieldSet`, which tracks the same four fields but is not
/// readable from outside its crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CollapsedFields(u8);

impl CollapsedFields {
    const RESOURCE: u8 = 1 << 0;
    const HTTP_ENDPOINT: u8 = 1 << 1;
    const PEER_TAGS: u8 = 1 << 2;
    const ADDITIONAL_TAGS: u8 = 1 << 3;
    const ALL: u8 = Self::RESOURCE | Self::HTTP_ENDPOINT | Self::PEER_TAGS | Self::ADDITIONAL_TAGS;

    fn add(&mut self, field: u8) {
        self.0 |= field;
    }

    fn is_saturated(self) -> bool {
        self.0 == Self::ALL
    }

    fn contains(self, field: u8) -> bool {
        self.0 & field != 0
    }

    /// Each field's bit, the noun to use when reporting it, and the limit that governs it.
    fn reportable(limits: &CardinalityLimitConfig) -> [(u8, &'static str, usize); 4] {
        [
            (Self::RESOURCE, "resource names", limits.resource_limit),
            (
                Self::HTTP_ENDPOINT,
                "HTTP endpoints",
                limits.http_endpoint_limit,
            ),
            (Self::PEER_TAGS, "peer tag sets", limits.peer_tags_limit),
            (
                Self::ADDITIONAL_TAGS,
                "additional metric tag sets",
                limits.additional_tags_limit,
            ),
        ]
    }
}

/// Scan a flushed payload for per-field cardinality collapse.
///
/// This is the only way bottlecap can see per-field collapse today: `FlushResult.collapsed_spans`
/// counts *whole-key* overflow exclusively, and the per-field counters
/// (`StatsBucket::collapsed_fields_metrics`) have no public reader without the `dogstatsd`/
/// `telemetry` features, which bottlecap does not enable. The payload itself carries the signal,
/// because collapse is visible in the emitted field values.
///
/// Per-field collapse rewrites *only* the field that exceeded its limit, whereas the whole-key
/// overflow entry has *every* field set to the sentinel. `service` is never rewritten by per-field
/// collapse, so it identifies that overflow entry and lets us skip it; otherwise a single
/// whole-key overflow would masquerade as all four fields collapsing at once.
fn observe_collapsed_fields(buckets: &[pb::ClientStatsBucket]) -> CollapsedFields {
    let mut observed = CollapsedFields::default();
    for stats in buckets.iter().flat_map(|bucket| &bucket.stats) {
        if stats.service == TRACER_BLOCKED_VALUE {
            continue;
        }
        if stats.resource == TRACER_BLOCKED_VALUE {
            observed.add(CollapsedFields::RESOURCE);
        }
        if stats.http_endpoint == TRACER_BLOCKED_VALUE {
            observed.add(CollapsedFields::HTTP_ENDPOINT);
        }
        if stats.peer_tags.iter().any(|tag| is_sentinel_tag(tag)) {
            observed.add(CollapsedFields::PEER_TAGS);
        }
        if stats
            .additional_metric_tags
            .iter()
            .any(|tag| is_sentinel_tag(tag))
        {
            observed.add(CollapsedFields::ADDITIONAL_TAGS);
        }
    }
    observed
}

/// Whether an encoded tag carries the collapse sentinel as its key.
///
/// The two tag lists encode differently: `peer_tags` emits a valueless tag as the bare key, while
/// `additional_metric_tags` always appends `:`. Comparing the key half handles both.
fn is_sentinel_tag(tag: &str) -> bool {
    tag.split_once(':').map_or(tag, |(key, _)| key) == TRACER_BLOCKED_VALUE
}

/// Maximum number of additional metric tag keys libdatadog will aggregate on.
///
/// TODO: mirrors `ADDITIONAL_METRIC_TAGS_MAX_KEYS` in libdatadog's
/// `libdd-trace-stats/src/span_concentrator/mod.rs`, which is private. Hand-copied here only to
/// warn about excess keys in bottlecap's own terms; libdatadog still owns the actual truncation.
const MAX_ADDITIONAL_METRIC_TAG_KEYS: usize = 4;

/// Build the `CardinalityLimitConfig` override for a user-supplied
/// `DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT`, or `None` to keep libdatadog's defaults.
///
/// libdatadog only warns about out-of-range limits, it still applies them, so validate here.
///
/// `0` is the dangerous one: libdatadog would collapse *every* additional tag into the
/// `tracer_blocked_value` sentinel. Note the Go trace agent reads `0` as "no cap" instead, so a
/// user carrying that setting over would otherwise silently lose every tag value. Falling back to
/// the default keeps aggregation working; "unbounded" is deliberately not offered, since #1332
/// bounded this precisely to cap concentrator memory in a memory-capped Lambda.
///
/// Values at or above `whole_key_limit` are clamped mainly to silence libdatadog's
/// misconfiguration warning. Per-field limits are applied *before* the whole-key limit, so such a
/// value is not strictly inert, but reaching it needs ~7k distinct tag combinations inside one
/// 10s bucket, which will not happen in a Lambda invocation.
fn resolve_cardinality_limits(configured_limit: Option<usize>) -> Option<CardinalityLimitConfig> {
    let defaults = CardinalityLimitConfig::default();
    // `saturating_sub` keeps the clamp below the whole-key limit so it stays effective.
    let max_effective_limit = defaults.whole_key_limit.saturating_sub(1);

    let additional_tags_limit = match configured_limit? {
        0 => {
            warn!(
                "DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT=0 would collapse all additional \
                 metric tags into `tracer_blocked_value`; using the default of {} instead. Note \
                 that 0 does not mean unlimited here; to stop aggregating on additional tags, \
                 unset DD_TRACE_STATS_ADDITIONAL_TAGS instead.",
                defaults.additional_tags_limit
            );
            return None;
        }
        limit if limit > max_effective_limit => {
            warn!(
                "DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT={limit} is at or above the \
                 whole-key cardinality limit ({}), so it is effectively unbounded; clamping to \
                 {max_effective_limit}.",
                defaults.whole_key_limit
            );
            max_effective_limit
        }
        limit => limit,
    };

    Some(CardinalityLimitConfig {
        additional_tags_limit,
        ..defaults
    })
}

/// Warn when `DD_TRACE_STATS_ADDITIONAL_TAGS` lists more keys than libdatadog will aggregate on.
///
/// libdatadog sorts alphabetically and keeps only the first
/// [`MAX_ADDITIONAL_METRIC_TAG_KEYS`], so excess keys are dropped by alphabetical accident
/// rather than by anything the user expressed. Its own warning names the dropped keys but not
/// the kept ones, the selection rule, or the env var, so restate all three here. Truncation
/// itself is left to libdatadog; this only reports it.
fn warn_on_excess_additional_metric_tag_keys(keys: &[String]) {
    if let Some((kept, dropped)) = split_additional_metric_tag_keys(keys) {
        warn!(
            "DD_TRACE_STATS_ADDITIONAL_TAGS lists {} unique keys but at most {} are aggregated \
             on. Keys are sorted alphabetically and the rest dropped, so stats will use {kept:?} \
             and ignore {dropped:?}. Reduce the list to at most {} keys to choose explicitly.",
            kept.len() + dropped.len(),
            MAX_ADDITIONAL_METRIC_TAG_KEYS,
            MAX_ADDITIONAL_METRIC_TAG_KEYS,
        );
    }
}

/// Split `keys` into the keys libdatadog will keep and the ones it will drop, or `None` when the
/// list is within [`MAX_ADDITIONAL_METRIC_TAG_KEYS`].
///
/// Mirrors libdatadog's `normalize_additional_metric_tag_keys` (sort, dedup, truncate) so the
/// split reported matches the split it will actually apply.
fn split_additional_metric_tag_keys(keys: &[String]) -> Option<(Vec<&str>, Vec<&str>)> {
    let mut normalized: Vec<&str> = keys.iter().map(String::as_str).collect();
    normalized.sort_unstable();
    normalized.dedup();

    if normalized.len() <= MAX_ADDITIONAL_METRIC_TAG_KEYS {
        return None;
    }
    let dropped = normalized.split_off(MAX_ADDITIONAL_METRIC_TAG_KEYS);
    Some((normalized, dropped))
}

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
    /// The limits the concentrator was built with, so collapse warnings can name the limit that
    /// fired instead of re-deriving it.
    cardinality_limits: CardinalityLimitConfig,
    /// Whether whole-key overflow has already been warned about, so it warns at most once per
    /// sandbox. A plain `bool` rather than an `AtomicBool` because `handle_flush` takes
    /// `&mut self`; only `StatsConcentratorHandle` is cloned and shared.
    whole_key_collapse_reported: bool,
    /// Same, per collapsed field.
    reported_collapsed_fields: CollapsedFields,
}

// A service that handles add() and flush() requests in the same queue,
// to avoid using mutex, which may cause lock contention.
impl StatsConcentratorService {
    #[must_use]
    pub fn new(config: Arc<Config>) -> (Self, StatsConcentratorHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = StatsConcentratorHandle::new(tx);
        warn_on_excess_additional_metric_tag_keys(&config.ext.additional_metric_tags);
        // Resolved once, here, so the limits the collapse warnings quote are the same values the
        // concentrator enforces. `unwrap_or_default()` mirrors what libdatadog does with a `None`
        // override.
        let cardinality_limits =
            resolve_cardinality_limits(config.ext.additional_metric_tags_cardinality_limit)
                .unwrap_or_default();
        let concentrator = SpanConcentrator::new(
            Duration::from_nanos(BUCKET_DURATION_NS),
            SystemTime::now(),
            STATS_ELIGIBLE_SPAN_KINDS
                .iter()
                .map(ToString::to_string)
                .collect(),
            DEFAULT_PEER_TAG_KEYS
                .iter()
                .map(ToString::to_string)
                .collect(),
            // Use libdatadog's default cardinality limits except for `additional_tags_limit`,
            // which is overridden by `DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT` when
            // set (matching the Serverless Compatibility Layer / `datadog-trace-agent`).
            // Defaults: 7000 whole-key, 1024 resource, 512 http endpoint, 512 peer tags, 100
            // additional tags. Keys beyond a limit collapse into the `tracer_blocked_value`
            // overflow bucket, which bounds concentrator memory and the /v0.6/stats payload
            // inside a memory-capped Lambda.
            //
            // Passed as `Some` of the resolved value rather than the raw `Option` (which
            // libdatadog would resolve with `unwrap_or_default()`, so the two are equivalent)
            // so that the limits the collapse warnings quote are provably the ones in force.
            Some(cardinality_limits),
            // Span meta keys included as additional aggregation dimensions, from
            // DD_TRACE_STATS_ADDITIONAL_TAGS (only set when experimental_features_enabled).
            config.ext.additional_metric_tags.clone(),
        );
        let service: StatsConcentratorService = Self {
            concentrator,
            rx,
            // To be set when the first trace is received
            tracer_metadata: TracerMetadata::default(),
            config,
            cardinality_limits,
            whole_key_collapse_reported: false,
            reported_collapsed_fields: CollapsedFields::default(),
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
        let flush_result = self.concentrator.flush(SystemTime::now(), force_flush);
        let collapsed_spans = flush_result.collapsed_spans;
        // Obfuscation is excluded at the feature level: bottlecap's `libdd-trace-stats`
        // dependency does not enable `stats-obfuscation`, so every bucket ends up in
        // `unobfuscated_buckets`; combine both to stay correct if that ever changes. Start
        // from `unobfuscated_buckets` since it's normally the only non-empty one, avoiding a
        // reallocation to grow the (usually empty) `obfuscated_buckets` vec.
        let mut stats_buckets = flush_result.unobfuscated_buckets;
        stats_buckets.extend(flush_result.obfuscated_buckets);
        self.report_collapse(&stats_buckets, collapsed_spans);
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

    /// Warn, at most once per sandbox per signal, when cardinality limits collapsed stats keys.
    ///
    /// Two independent signals are needed because they fail in opposite directions: per-field
    /// limits are applied before the whole-key limit, so a single-dimension explosion (for
    /// example request ids in resource names) can collapse resources without `collapsed_spans`
    /// ever leaving 0. Relying on `collapsed_spans` alone would be silent for exactly the case
    /// this reporting exists to surface, so per-field collapse is also detected by scanning the
    /// payload for the sentinel value.
    ///
    /// No per-flush `debug!`: libdatadog already emits one for whole-key overflow.
    /// `StatsBucket::collapsed_fields_metrics()` is not consulted because its per-combination
    /// counts have no public accessor without the `dogstatsd`/`telemetry` features; it can only
    /// report that something collapsed, which the payload scan already does.
    fn report_collapse(&mut self, buckets: &[pb::ClientStatsBucket], collapsed_spans: u64) {
        // Every signal has already warned, so nothing below can produce output. Worth an early
        // return because the payload scan is not free: a bucket can hold thousands of entries and
        // flushes are frequent under continuous flushing.
        if self.whole_key_collapse_reported && self.reported_collapsed_fields.is_saturated() {
            return;
        }

        if collapsed_spans > 0 && !self.whole_key_collapse_reported {
            self.whole_key_collapse_reported = true;
            warn!(
                "Trace stats exceeded the per-bucket limit of {} distinct aggregation keys; \
                 {collapsed_spans} span(s) in this flush were aggregated under the \
                 '{TRACER_BLOCKED_VALUE}' overflow key and are no longer attributable to a \
                 service, resource or endpoint. This limit is not configurable; reduce span \
                 resource and endpoint cardinality to keep trace stats accurate. Warned once per \
                 sandbox.",
                self.cardinality_limits.whole_key_limit
            );
        }

        // Names no environment variable, deliberately: the per-field limits are not
        // customer-tunable in bottlecap, and both candidate knobs would mislead. libdatadog's own
        // message blames `DD_TRACE_STATS_CARDINALITY_LIMIT`, which bottlecap does not read at all,
        // and `DD_TRACE_STATS_ADDITIONAL_TAGS_CARDINALITY_LIMIT` governs only `additional_tags`
        // (and only once the additional-tags feature is enabled). Reducing cardinality in the
        // application is the only real remediation, so that is what this recommends.
        let bucket_secs = Duration::from_nanos(BUCKET_DURATION_NS).as_secs();
        let observed = observe_collapsed_fields(buckets);
        for (field, noun, limit) in CollapsedFields::reportable(&self.cardinality_limits) {
            if !observed.contains(field) || self.reported_collapsed_fields.contains(field) {
                continue;
            }
            self.reported_collapsed_fields.add(field);
            warn!(
                "Trace stats saw more than {limit} distinct {noun} in a {bucket_secs}s bucket; \
                 the excess is aggregated under '{TRACER_BLOCKED_VALUE}', so those stats are no \
                 longer attributable. Reduce cardinality to keep trace stats accurate; request \
                 ids or path parameters embedded in resource names are the usual cause. Warned \
                 once per sandbox."
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Create a `pb::Span` with the given meta tags and metrics.
    /// The span is non-root (`parent_id=1`) and not measured, so it will only be
    /// eligible for stats if `span_kinds_stats_computed` includes its `span.kind`.
    fn create_span_kind_span(span_kind: &str, meta: Vec<(&str, &str)>) -> pb::Span {
        create_span_kind_span_with_resource(span_kind, "test-resource", meta)
    }

    /// Same as `create_span_kind_span`, but with a caller-provided resource name so tests can
    /// generate many distinct aggregation keys.
    fn create_span_kind_span_with_resource(
        span_kind: &str,
        resource: &str,
        meta: Vec<(&str, &str)>,
    ) -> pb::Span {
        let now_ns = i64::try_from(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        )
        .unwrap();
        let mut meta_map: HashMap<String, String> = meta
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        meta_map.insert("span.kind".to_string(), span_kind.to_string());
        pb::Span {
            service: "test-service".to_string(),
            name: "test-op".to_string(),
            resource: resource.to_string(),
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

    /// A non-root, non-measured span with `span.kind`="client" should produce stats
    /// because `span_kinds_stats_computed` is populated with the eligible span kinds.
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

    /// A client span with peer tag meta keys (`db.instance`, `db.system`) should produce
    /// stats with non-empty `peer_tags` because `peer_tag_keys` is configured.
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
            "Expected stats for a client span with peer tags, but got None."
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

    /// `additional_metric_tags` (populated from `DD_TRACE_STATS_ADDITIONAL_TAGS`, gated on
    /// `DD_TRACE_EXPERIMENTAL_FEATURES_ENABLED`) should surface matching span `meta` keys as
    /// `ClientGroupedStats.additional_metric_tags` on export.
    #[tokio::test]
    async fn test_additional_metric_tags_populated_when_configured() {
        let mut config = Config::default();
        config.ext.additional_metric_tags = vec!["datacenter".to_string()];
        let config = Arc::new(config);
        let (service, handle) = StatsConcentratorService::new(config);
        tokio::spawn(service.run());

        let span = create_span_kind_span("client", vec![("datacenter", "us-east-1")]);
        handle.add(&span).unwrap();

        let result = handle.flush(true).await.unwrap();
        let payload = result.expect("Expected stats for the client span, but got None.");
        let all_stats: Vec<_> = payload.stats.iter().flat_map(|b| &b.stats).collect();
        assert!(
            all_stats
                .iter()
                .any(|s| s.additional_metric_tags == vec!["datacenter:us-east-1".to_string()]),
            "Expected additional_metric_tags to contain datacenter:us-east-1, got: {:?}",
            all_stats
                .iter()
                .map(|s| &s.additional_metric_tags)
                .collect::<Vec<_>>()
        );
    }

    /// When `additional_metric_tags` is unset (the default), `additional_metric_tags` on the
    /// exported stats must remain empty even if the span has a meta key that would otherwise
    /// match a commonly-used tag name.
    #[tokio::test]
    async fn test_additional_metric_tags_empty_by_default() {
        let config = Arc::new(Config::default());
        let (service, handle) = StatsConcentratorService::new(config);
        tokio::spawn(service.run());

        let span = create_span_kind_span("client", vec![("datacenter", "us-east-1")]);
        handle.add(&span).unwrap();

        let result = handle.flush(true).await.unwrap();
        let payload = result.expect("Expected stats for the client span, but got None.");
        let all_stats: Vec<_> = payload.stats.iter().flat_map(|b| &b.stats).collect();
        assert!(
            all_stats
                .iter()
                .all(|s| s.additional_metric_tags.is_empty()),
            "Expected additional_metric_tags to be empty by default, got: {:?}",
            all_stats
                .iter()
                .map(|s| &s.additional_metric_tags)
                .collect::<Vec<_>>()
        );
    }

    /// libdatadog only warns about out-of-range cardinality limits and still applies them, so
    /// `resolve_cardinality_limits` has to reject the two misconfigurations that would silently
    /// break stats: `0` (collapses every additional tag) and any value at or above the whole-key
    /// limit (inert, because the whole-key limit collapses the key first).
    #[test]
    fn test_resolve_cardinality_limits() {
        let defaults = CardinalityLimitConfig::default();

        // Unset: keep libdatadog's defaults entirely.
        assert_eq!(resolve_cardinality_limits(None), None);

        // 0 would collapse everything, fall back to the defaults.
        assert_eq!(resolve_cardinality_limits(Some(0)), None);

        // In-range values are applied, leaving the other limits at their defaults.
        let resolved = resolve_cardinality_limits(Some(5)).expect("expected an override");
        assert_eq!(resolved.additional_tags_limit, 5);
        assert_eq!(resolved.whole_key_limit, defaults.whole_key_limit);
        assert_eq!(resolved.resource_limit, defaults.resource_limit);

        // At or above the whole-key limit is clamped so it stays effective.
        let clamped = resolve_cardinality_limits(Some(defaults.whole_key_limit))
            .expect("expected an override");
        assert_eq!(clamped.additional_tags_limit, defaults.whole_key_limit - 1);
        let clamped_high =
            resolve_cardinality_limits(Some(usize::MAX)).expect("expected an override");
        assert_eq!(
            clamped_high.additional_tags_limit,
            defaults.whole_key_limit - 1
        );
    }

    /// libdatadog silently keeps only the first `MAX_ADDITIONAL_METRIC_TAG_KEYS` keys after
    /// sorting alphabetically, so which keys survive is an alphabetical accident rather than
    /// anything the user expressed. Verify the split we report matches that rule.
    #[test]
    fn test_split_additional_metric_tag_keys() {
        let keys =
            |keys: &[&str]| -> Vec<String> { keys.iter().map(ToString::to_string).collect() };

        // Within the cap: nothing is dropped.
        assert_eq!(split_additional_metric_tag_keys(&[]), None);
        assert_eq!(
            split_additional_metric_tag_keys(&keys(&["region", "shard", "zone", "tenant_id"])),
            None
        );

        // Duplicates collapse first, so this stays within the cap.
        assert_eq!(
            split_additional_metric_tag_keys(&keys(&["region", "region", "shard"])),
            None
        );

        // Over the cap: alphabetical order decides, so `zone` loses despite being listed first.
        assert_eq!(
            split_additional_metric_tag_keys(&keys(&[
                "zone",
                "tenant_id",
                "region",
                "shard",
                "customer"
            ])),
            Some((
                vec!["customer", "region", "shard", "tenant_id"],
                vec!["zone"]
            ))
        );
    }

    /// The concentrator uses `CardinalityLimitConfig::default()`, so exceeding those limits must
    /// collapse the excess aggregation keys into the `tracer_blocked_value` overflow key instead
    /// of growing without bound. 7,001 distinct resources exceeds both the default
    /// `whole_key_limit` (7,000) and `resource_limit` (1,024).
    #[tokio::test]
    async fn test_cardinality_limit_applied() {
        const OVERFLOW_KEY: &str = TRACER_BLOCKED_VALUE;
        let span_count = CardinalityLimitConfig::default().whole_key_limit + 1;

        let config = Arc::new(Config::default());
        let (service, handle) = StatsConcentratorService::new(config);
        tokio::spawn(service.run());

        for i in 0..span_count {
            let resource = format!("test-resource-{i}");
            let span = create_span_kind_span_with_resource("client", &resource, vec![]);
            handle.add(&span).unwrap();
        }

        let result = handle.flush(true).await.unwrap();

        let payload = result.expect("Expected stats for the generated spans, but got None.");
        let all_stats: Vec<_> = payload.stats.iter().flat_map(|b| &b.stats).collect();
        assert!(
            all_stats.len() < span_count,
            "Expected fewer stats entries than distinct resources once the resource limit \
             collapses the excess, got {} for {span_count} resources.",
            all_stats.len()
        );
        assert!(
            all_stats.iter().any(|s| s.resource == OVERFLOW_KEY),
            "Expected the resources beyond the limit to collapse into the '{OVERFLOW_KEY}' \
             overflow key."
        );
    }

    /// `(service, resource, http_endpoint, peer_tags, additional_metric_tags)`
    type StatsEntry<'a> = (&'a str, &'a str, &'a str, Vec<&'a str>, Vec<&'a str>);

    /// Build a payload bucket from [`StatsEntry`] tuples.
    fn bucket(entries: Vec<StatsEntry<'_>>) -> pb::ClientStatsBucket {
        pb::ClientStatsBucket {
            stats: entries
                .into_iter()
                .map(
                    |(service, resource, http_endpoint, peer_tags, additional_metric_tags)| {
                        pb::ClientGroupedStats {
                            service: service.to_string(),
                            resource: resource.to_string(),
                            http_endpoint: http_endpoint.to_string(),
                            peer_tags: peer_tags.into_iter().map(ToString::to_string).collect(),
                            additional_metric_tags: additional_metric_tags
                                .into_iter()
                                .map(ToString::to_string)
                                .collect(),
                            ..Default::default()
                        }
                    },
                )
                .collect(),
            ..Default::default()
        }
    }

    /// Per-field collapse rewrites only the field that overflowed, so each field is detected
    /// independently. The whole-key overflow entry, which sets *every* field to the sentinel, must
    /// not be mistaken for all four collapsing at once.
    #[test]
    fn test_observe_collapsed_fields() {
        const S: &str = TRACER_BLOCKED_VALUE;

        // Nothing collapsed.
        assert_eq!(
            observe_collapsed_fields(&[bucket(vec![(
                "svc",
                "GET /users",
                "/users",
                vec!["peer.service:db"],
                vec!["region:ca-central-1"]
            )])]),
            CollapsedFields::default()
        );

        // Resource collapsed on its own: the field that overflowed, and only that field.
        assert_eq!(
            observe_collapsed_fields(&[bucket(vec![("svc", S, "/users", vec![], vec![])])]),
            CollapsedFields(CollapsedFields::RESOURCE)
        );

        // The whole-key overflow entry is identified by `service` (never rewritten per-field)
        // and skipped, so it reports no per-field collapse at all.
        assert_eq!(
            observe_collapsed_fields(&[bucket(vec![(
                S,
                S,
                S,
                vec![S],
                vec!["tracer_blocked_value:"]
            )])]),
            CollapsedFields::default()
        );

        // Both tag lists, which encode a valueless sentinel differently: `peer_tags` as the bare
        // key, `additional_metric_tags` with a trailing colon.
        assert_eq!(
            observe_collapsed_fields(&[bucket(vec![(
                "svc",
                "GET /users",
                S,
                vec![S],
                vec!["tracer_blocked_value:"]
            )])]),
            CollapsedFields(
                CollapsedFields::HTTP_ENDPOINT
                    | CollapsedFields::PEER_TAGS
                    | CollapsedFields::ADDITIONAL_TAGS
            )
        );

        // Collapse in any bucket of the flush counts.
        assert_eq!(
            observe_collapsed_fields(&[
                bucket(vec![("svc", "GET /users", "/users", vec![], vec![])]),
                bucket(vec![("svc", S, "/users", vec![], vec![])]),
            ]),
            CollapsedFields(CollapsedFields::RESOURCE)
        );
    }

    /// Regression test for the observability trap: a single-dimension resource explosion collapses
    /// per-field, which shrinks the whole-key space so `collapsed_spans` never fires. Exceeding
    /// `resource_limit` (1,024) while staying well under `whole_key_limit` (7,000) must therefore
    /// still be observable: via the payload scan, and *not* via a whole-key overflow entry.
    #[tokio::test]
    async fn test_resource_collapse_observed_without_whole_key_overflow() {
        let limits = CardinalityLimitConfig::default();
        let span_count = limits.resource_limit + 1;
        assert!(
            span_count < limits.whole_key_limit,
            "This test is only meaningful below the whole-key limit."
        );

        let config = Arc::new(Config::default());
        let (service, handle) = StatsConcentratorService::new(config);
        tokio::spawn(service.run());

        for i in 0..span_count {
            let resource = format!("GET /users/{i}");
            let span = create_span_kind_span_with_resource("client", &resource, vec![]);
            handle.add(&span).unwrap();
        }

        let payload = handle
            .flush(true)
            .await
            .unwrap()
            .expect("Expected stats for the generated spans, but got None.");

        assert_eq!(
            observe_collapsed_fields(&payload.stats),
            CollapsedFields(CollapsedFields::RESOURCE),
            "Exceeding the resource limit must be observable as a resource collapse."
        );
        assert!(
            !payload
                .stats
                .iter()
                .flat_map(|bucket| &bucket.stats)
                .any(|stats| stats.service == TRACER_BLOCKED_VALUE),
            "No whole-key overflow entry should exist here; if one does, this test no longer \
             covers the per-field-only case."
        );
    }

    /// Each signal warns at most once per sandbox, and the two are independent: whole-key
    /// overflow must not suppress a later per-field collapse or vice versa. Asserted on the
    /// service's own state rather than on log output; `report_collapse` logs iff it flips a flag.
    ///
    /// This also covers the whole-key branch, which is otherwise unreachable in tests: `new()`
    /// always builds the concentrator with libdatadog's defaults, and driving distinct keys past
    /// `whole_key_limit` (7,000) is impossible without first tripping `resource_limit` (1,024),
    /// which collapses per-field and keeps `collapsed_spans` at 0.
    #[tokio::test]
    async fn test_collapse_warns_once_per_signal() {
        let config = Arc::new(Config::default());
        let (mut service, _handle) = StatsConcentratorService::new(config);

        assert!(!service.whole_key_collapse_reported);
        assert_eq!(
            service.reported_collapsed_fields,
            CollapsedFields::default()
        );

        // No collapse at all: nothing is reported.
        service.report_collapse(&[], 0);
        assert!(!service.whole_key_collapse_reported);

        // Whole-key overflow, reported from the span count, with no per-field collapse present.
        service.report_collapse(&[], 3);
        assert!(service.whole_key_collapse_reported);
        assert_eq!(
            service.reported_collapsed_fields,
            CollapsedFields::default()
        );

        // A per-field collapse in a later flush is still reported, independently.
        let collapsed = [bucket(vec![(
            "svc",
            TRACER_BLOCKED_VALUE,
            "/users",
            vec![],
            vec![],
        )])];
        service.report_collapse(&collapsed, 9);
        assert_eq!(
            service.reported_collapsed_fields,
            CollapsedFields(CollapsedFields::RESOURCE)
        );

        // Repeat flushes take the early return, leaving state untouched.
        service.report_collapse(&collapsed, 9);
        assert_eq!(
            service.reported_collapsed_fields,
            CollapsedFields(CollapsedFields::RESOURCE)
        );
    }
}
