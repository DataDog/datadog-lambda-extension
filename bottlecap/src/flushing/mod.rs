//! Flushing service module for coordinating data flushes across multiple flusher types.
//!
//! This module provides a unified `FlushingService` that orchestrates flushing of:
//! - Logs (via `LogsFlusher`)
//! - Traces (via `TraceFlusher`)
//! - Stats (via `StatsFlusher`)
//! - Metrics (via `MetricsFlusher`)
//! - Proxy requests (via `ProxyFlusher`)

mod handles;
mod service;

pub use handles::{FlushHandles, MetricsRetryBatch};
pub use service::FlushingService;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Shared, hot-swappable current invocation deadline (epoch ms).
///
/// Stored as `0` when no deadline is known (e.g. before the first INVOKE event,
/// or after SHUTDOWN). Updated on every Invoke event in `handle_next_invocation`.
/// Read by each flusher's `send()` to compute an adaptive per-request timeout.
pub type InvocationDeadline = Arc<AtomicU64>;

/// Returns a new `InvocationDeadline` initialized to `0` (no deadline known).
#[must_use]
pub fn new_invocation_deadline() -> InvocationDeadline {
    Arc::new(AtomicU64::new(0))
}

/// Returns the effective per-request flush timeout, honoring both the static
/// `flush_timeout` ceiling and the Lambda invocation deadline.
///
/// Formula: `min(flush_timeout_secs, deadline_ms - now_ms - margin_ms)`,
/// saturating to `Duration::ZERO` when the deadline has already passed (or is
/// unset, indicated by `deadline_ms == 0`).
///
/// Callers should treat `Duration::ZERO` as "no budget — skip this send and
/// re-enqueue."
#[must_use]
pub fn compute_flush_cap(
    deadline_ms: u64,
    flush_timeout_secs: u64,
    margin_ms: u64,
) -> Duration {
    let ceiling = Duration::from_secs(flush_timeout_secs);
    if deadline_ms == 0 {
        return ceiling;
    }
    let now_ms = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
    )
    .unwrap_or(u64::MAX);
    let remaining_ms = deadline_ms
        .saturating_sub(now_ms)
        .saturating_sub(margin_ms);
    let remaining = Duration::from_millis(remaining_ms);
    std::cmp::min(ceiling, remaining)
}

/// Convenience: load `deadline_ms` from the shared atomic and compute the cap.
#[must_use]
pub fn compute_flush_cap_from(
    deadline: &InvocationDeadline,
    flush_timeout_secs: u64,
    margin_ms: u64,
) -> Duration {
    compute_flush_cap(
        deadline.load(Ordering::Relaxed),
        flush_timeout_secs,
        margin_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_uses_ceiling_when_no_deadline() {
        let cap = compute_flush_cap(0, 30, 100);
        assert_eq!(cap, Duration::from_secs(30));
    }

    #[test]
    fn cap_uses_remaining_when_tighter_than_ceiling() {
        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        // 5 seconds in the future, 100ms margin → 4900ms remaining, which is tighter than 30s.
        let cap = compute_flush_cap(now_ms + 5_000, 30, 100);
        // Allow a few ms of slop for clock movement between the two `now()` calls.
        assert!(cap <= Duration::from_millis(4_900));
        assert!(cap >= Duration::from_millis(4_800));
    }

    #[test]
    fn cap_saturates_to_zero_when_deadline_passed() {
        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        // Deadline already passed.
        let cap = compute_flush_cap(now_ms.saturating_sub(1_000), 30, 100);
        assert_eq!(cap, Duration::ZERO);
    }

    #[test]
    fn cap_saturates_to_zero_when_remaining_less_than_margin() {
        let now_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        // 50ms remaining, 100ms margin → 0.
        let cap = compute_flush_cap(now_ms + 50, 30, 100);
        assert_eq!(cap, Duration::ZERO);
    }
}
