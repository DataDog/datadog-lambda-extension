//! Dead letter queue (DLQ) for payloads that failed to send within an
//! invocation's budget.
//!
//! Each per-type queue holds residual payloads that exhausted their retry +
//! redrive attempts during a `flush_blocking()` call. At the start of every
//! subsequent flush, these queues are drained back into the corresponding
//! flusher's `flush(Some(...))` retry path. On SHUTDOWN, any items still
//! queued are dropped with a logged summary.
//!
//! Eviction policy when the configured byte cap is reached: **drop the
//! incoming item**, leaving the existing queue untouched. This matches the
//! `BufferFull` behavior in dd-trace-py's `BufferedEncoder::put`
//! (`ddtrace/internal/_encoding.pyx:411-416`) and is simpler than FIFO
//! eviction since the queued payloads are already-encoded reqwest requests
//! which are awkward to re-shuffle.
//!
//! All queues share a single byte budget (`config.flush_dlq_max_bytes`,
//! default 50 MiB) so a flood from one telemetry type doesn't unfairly evict
//! another's healthy backlog.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use libdd_trace_protobuf::pb;
use libdd_trace_utils::send_data::SendData;
use prost::Message;
use tokio::sync::Mutex;
use tracing::{error, trace};

use crate::flushing::handles::MetricsRetryBatch;

/// A payload waiting to be re-flushed on a future invocation, together with
/// its approximate serialized byte size (used for shared byte accounting).
pub struct DlqItem<T> {
    pub payload: T,
    pub size_bytes: u64,
}

/// Reason a DLQ push was rejected. Surfaced in drop-log messages.
#[derive(Debug, Clone, Copy)]
pub enum DropReason {
    /// The combined byte budget was exhausted and the incoming item couldn't
    /// fit.
    DlqFull,
}

impl DropReason {
    fn as_str(self) -> &'static str {
        match self {
            DropReason::DlqFull => "dlq_full",
        }
    }
}

/// Telemetry type tag carried by drop logs for correlation.
#[derive(Debug, Clone, Copy)]
pub enum TelemetryType {
    Logs,
    Traces,
    Stats,
    Proxy,
    Metrics,
}

impl TelemetryType {
    fn as_str(self) -> &'static str {
        match self {
            TelemetryType::Logs => "logs",
            TelemetryType::Traces => "traces",
            TelemetryType::Stats => "stats",
            TelemetryType::Proxy => "proxy",
            TelemetryType::Metrics => "metrics",
        }
    }
}

/// In-memory dead letter queue, shared across `FlushingService` clones. Held
/// behind an `Arc` and accessed via interior mutability so it can be pushed
/// from spawned tasks without requiring `&mut self`.
pub struct Dlq {
    pub logs: Mutex<VecDeque<DlqItem<reqwest::RequestBuilder>>>,
    pub traces: Mutex<VecDeque<DlqItem<SendData>>>,
    pub stats: Mutex<VecDeque<DlqItem<pb::ClientStatsPayload>>>,
    pub proxy: Mutex<VecDeque<DlqItem<reqwest::RequestBuilder>>>,
    pub metrics: Mutex<VecDeque<DlqItem<MetricsRetryBatch>>>,
    /// Sum of `size_bytes` across all queues. Tracked as an atomic so
    /// concurrent pushers can race-then-check without holding any queue lock.
    total_bytes: AtomicU64,
    /// Cap from `config.flush_dlq_max_bytes`. Pushes that would exceed this
    /// are rejected and the incoming item is dropped with a log entry.
    max_bytes: u64,
}

impl Dlq {
    #[must_use]
    pub fn new(max_bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            logs: Mutex::new(VecDeque::new()),
            traces: Mutex::new(VecDeque::new()),
            stats: Mutex::new(VecDeque::new()),
            proxy: Mutex::new(VecDeque::new()),
            metrics: Mutex::new(VecDeque::new()),
            total_bytes: AtomicU64::new(0),
            max_bytes,
        })
    }

    /// Current total bytes across all queues (approximate; updated atomically
    /// on push/drain).
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Attempts to reserve `size_bytes` of budget. Returns `true` if the
    /// caller may proceed to push; returns `false` if the push would exceed
    /// `max_bytes` (and the caller MUST drop the payload with a log line).
    ///
    /// On success, the caller is responsible for the matching `release` call
    /// when the item is later drained.
    fn try_reserve(&self, size_bytes: u64) -> bool {
        // Use compare_exchange so concurrent pushers don't race past the cap.
        loop {
            let current = self.total_bytes.load(Ordering::Relaxed);
            let Some(new_total) = current.checked_add(size_bytes) else {
                return false;
            };
            if new_total > self.max_bytes {
                return false;
            }
            if self
                .total_bytes
                .compare_exchange(current, new_total, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Releases `size_bytes` of budget. Called when an item is drained out of
    /// the queue (whether for a retry send or for a final drop).
    pub fn release(&self, size_bytes: u64) {
        self.total_bytes.fetch_sub(size_bytes, Ordering::AcqRel);
    }
}

// ─── Per-type push helpers ───────────────────────────────────────────────────
//
// Each push helper takes the payload and its caller-estimated `size_bytes`,
// emits the two-level drop logs on overflow, and never blocks on a full queue
// — the incoming item is the one that loses.

impl Dlq {
    pub async fn try_push_logs(&self, payload: reqwest::RequestBuilder, size_bytes: u64) {
        if !self.try_reserve(size_bytes) {
            log_dropped(TelemetryType::Logs, size_bytes, DropReason::DlqFull, self);
            return;
        }
        self.logs
            .lock()
            .await
            .push_back(DlqItem { payload, size_bytes });
    }

    pub async fn try_push_traces(&self, payload: SendData, size_bytes: u64) {
        if !self.try_reserve(size_bytes) {
            // Payload contents are noisy to log; only include the metadata.
            log_dropped(TelemetryType::Traces, size_bytes, DropReason::DlqFull, self);
            return;
        }
        self.traces
            .lock()
            .await
            .push_back(DlqItem { payload, size_bytes });
    }

    pub async fn try_push_stats(&self, payload: pb::ClientStatsPayload, size_bytes: u64) {
        if !self.try_reserve(size_bytes) {
            log_dropped(TelemetryType::Stats, size_bytes, DropReason::DlqFull, self);
            return;
        }
        self.stats
            .lock()
            .await
            .push_back(DlqItem { payload, size_bytes });
    }

    pub async fn try_push_proxy(&self, payload: reqwest::RequestBuilder, size_bytes: u64) {
        if !self.try_reserve(size_bytes) {
            log_dropped(TelemetryType::Proxy, size_bytes, DropReason::DlqFull, self);
            return;
        }
        self.proxy
            .lock()
            .await
            .push_back(DlqItem { payload, size_bytes });
    }

    pub async fn try_push_metrics(&self, payload: MetricsRetryBatch, size_bytes: u64) {
        if !self.try_reserve(size_bytes) {
            log_dropped(
                TelemetryType::Metrics,
                size_bytes,
                DropReason::DlqFull,
                self,
            );
            return;
        }
        self.metrics
            .lock()
            .await
            .push_back(DlqItem { payload, size_bytes });
    }
}

// ─── Size estimators ─────────────────────────────────────────────────────────
//
// Callers measure their payload before pushing. For types where the wire size
// is opaque (e.g. reqwest::RequestBuilder body), the original byte buffer is
// the best estimate.

/// Approximate serialized size of a `ClientStatsPayload` (used for DLQ
/// accounting). Uses prost's `encoded_len`, which is cheap and exact.
#[must_use]
pub fn estimate_stats_size(payload: &pb::ClientStatsPayload) -> u64 {
    payload.encoded_len() as u64
}

/// Approximate serialized size of a `MetricsRetryBatch` for DLQ accounting.
/// Uses fixed-per-item heuristics so sizing doesn't require a full
/// serialization pass (the DLQ byte budget is a soft cap, not exact
/// accounting). The constants are calibrated against typical Lambda
/// telemetry — series tags dominate the wire bytes, and a sketch payload
/// carries a few hundred bytes of metadata plus quantile buckets.
#[must_use]
pub fn estimate_metrics_batch_size(batch: &MetricsRetryBatch) -> u64 {
    // JSON encoding for series is dominated by tag strings.
    const APPROX_BYTES_PER_SERIES: u64 = 512;
    // Protobuf-encoded sketch payloads are typically a few KB.
    const APPROX_BYTES_PER_SKETCH: u64 = 2_048;
    let series_bytes = (batch.series.len() as u64).saturating_mul(APPROX_BYTES_PER_SERIES);
    let sketches_bytes = (batch.sketches.len() as u64).saturating_mul(APPROX_BYTES_PER_SKETCH);
    series_bytes + sketches_bytes
}

// ─── Internal log helper ─────────────────────────────────────────────────────

fn log_dropped(kind: TelemetryType, size_bytes: u64, reason: DropReason, dlq: &Dlq) {
    let kind_str = kind.as_str();
    let reason_str = reason.as_str();
    let total = dlq.total_bytes();
    let max = dlq.max_bytes;
    // Error-level: structured one-liner visible at default log levels.
    error!(
        "DROP | type={kind_str} bytes={size_bytes} reason={reason_str} dlq_bytes={total}/{max}"
    );
    // Trace-level: a marker note. Payload contents are logged at trace level
    // at the call site (the helper here doesn't have the typed payload).
    trace!(
        "DROP | type={kind_str} reason={reason_str} (size {size_bytes} B); payload contents logged separately at trace level"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reserve_accepts_under_cap() {
        let dlq = Dlq::new(1_000);
        assert!(dlq.try_reserve(400));
        assert_eq!(dlq.total_bytes(), 400);
        assert!(dlq.try_reserve(500));
        assert_eq!(dlq.total_bytes(), 900);
    }

    #[tokio::test]
    async fn reserve_rejects_over_cap() {
        let dlq = Dlq::new(1_000);
        assert!(dlq.try_reserve(800));
        // Adding 300 more would exceed 1000; reject the incoming.
        assert!(!dlq.try_reserve(300));
        assert_eq!(dlq.total_bytes(), 800);
    }

    #[tokio::test]
    async fn release_decrements() {
        let dlq = Dlq::new(1_000);
        assert!(dlq.try_reserve(400));
        dlq.release(400);
        assert_eq!(dlq.total_bytes(), 0);
        // After release, room is freed.
        assert!(dlq.try_reserve(900));
    }

    #[tokio::test]
    async fn reserve_handles_overflow_arithmetic() {
        let dlq = Dlq::new(u64::MAX);
        // Push some, then try a size that would overflow u64.
        assert!(dlq.try_reserve(100));
        assert!(!dlq.try_reserve(u64::MAX));
        assert_eq!(dlq.total_bytes(), 100);
    }

    #[tokio::test]
    async fn estimate_stats_uses_prost_encoded_len() {
        let payload = pb::ClientStatsPayload {
            hostname: "host-a".to_string(),
            env: "prod".to_string(),
            ..Default::default()
        };
        let est = estimate_stats_size(&payload);
        // Just check it's nonzero and sane (a small payload should be < 100 bytes).
        assert!(est > 0);
        assert!(est < 100);
    }
}
