//! Flush handles for tracking in-flight flush operations.

use datadog_protos::metrics::SketchPayload;
use dogstatsd::datadog::Series;
use libdd_trace_utils::send_data::SendData;
use tokio::task::JoinHandle;

/// Batch of metrics that failed to flush and need to be retried.
#[derive(Debug)]
pub struct MetricsRetryBatch {
    /// Index of the flusher in the metrics flushers vector.
    pub flusher_id: usize,
    /// Series that failed to flush.
    pub series: Vec<Series>,
    /// Sketch payloads that failed to flush.
    pub sketches: Vec<SketchPayload>,
}

/// Handles for pending flush operations.
///
/// Each field contains a vector of `JoinHandle`s for in-flight flush tasks.
/// The return type of each handle contains data that may need to be retried
/// if the flush failed.
#[derive(Default)]
#[allow(clippy::struct_field_names)]
pub struct FlushHandles {
    /// Handles for trace flush operations. Returns failed traces for retry.
    pub trace_flush_handles: Vec<JoinHandle<Vec<SendData>>>,
    /// Handles for log flush operations. Returns failed request builders for retry.
    pub log_flush_handles: Vec<JoinHandle<Vec<reqwest::RequestBuilder>>>,
    /// Handles for metrics flush operations. Returns batch info for retry.
    pub metric_flush_handles: Vec<JoinHandle<MetricsRetryBatch>>,
    /// Handles for proxy flush operations. Returns failed request builders for retry.
    pub proxy_flush_handles: Vec<JoinHandle<Vec<reqwest::RequestBuilder>>>,
    /// Handles for stats flush operations. Stats don't support retry.
    pub stats_flush_handles: Vec<JoinHandle<()>>,
}

impl FlushHandles {
    /// Creates a new empty `FlushHandles` instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            trace_flush_handles: Vec::new(),
            log_flush_handles: Vec::new(),
            metric_flush_handles: Vec::new(),
            proxy_flush_handles: Vec::new(),
            stats_flush_handles: Vec::new(),
        }
    }

    /// Returns `true` if any flush operation is still pending (not finished).
    #[must_use]
    pub fn has_pending(&self) -> bool {
        let trace_pending = self.trace_flush_handles.iter().any(|h| !h.is_finished());
        let log_pending = self.log_flush_handles.iter().any(|h| !h.is_finished());
        let metric_pending = self.metric_flush_handles.iter().any(|h| !h.is_finished());
        let proxy_pending = self.proxy_flush_handles.iter().any(|h| !h.is_finished());
        let stats_pending = self.stats_flush_handles.iter().any(|h| !h.is_finished());

        trace_pending || log_pending || metric_pending || proxy_pending || stats_pending
    }

    /// Returns `true` if all handle vectors are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.trace_flush_handles.is_empty()
            && self.log_flush_handles.is_empty()
            && self.metric_flush_handles.is_empty()
            && self.proxy_flush_handles.is_empty()
            && self.stats_flush_handles.is_empty()
    }
}

impl std::fmt::Debug for FlushHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlushHandles")
            .field("trace_handles_count", &self.trace_flush_handles.len())
            .field("log_handles_count", &self.log_flush_handles.len())
            .field("metric_handles_count", &self.metric_flush_handles.len())
            .field("proxy_handles_count", &self.proxy_flush_handles.len())
            .field("stats_handles_count", &self.stats_flush_handles.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_handles_is_empty() {
        let handles = FlushHandles::new();
        assert!(handles.is_empty());
        assert!(!handles.has_pending());
    }

    #[test]
    fn test_default_handles_is_empty() {
        let handles = FlushHandles::default();
        assert!(handles.is_empty());
    }
}
