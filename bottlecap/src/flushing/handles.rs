//! Flush handles for tracking in-flight flush operations.

use libdd_trace_protobuf::pb;
use libdd_trace_utils::send_data::SendData;
use tokio::task::JoinHandle;

/// Handles for pending flush operations.
///
/// Each field contains a vector of `JoinHandle`s for in-flight flush tasks.
/// The return type of each handle contains data that may need to be retried
/// if the flush failed.
///
/// Note: metrics flushing is handled entirely by the Saluki topology (encoder +
/// forwarder with built-in retries), so no metric flush handles are needed here.
#[derive(Default)]
#[allow(clippy::struct_field_names)]
pub struct FlushHandles {
    /// Handles for trace flush operations. Returns failed traces for retry.
    pub trace_flush_handles: Vec<JoinHandle<Vec<SendData>>>,
    /// Handles for log flush operations. Returns failed request builders for retry.
    pub log_flush_handles: Vec<JoinHandle<Vec<reqwest::RequestBuilder>>>,
    /// Handles for proxy flush operations. Returns failed request builders for retry.
    pub proxy_flush_handles: Vec<JoinHandle<Vec<reqwest::RequestBuilder>>>,
    /// Handles for stats flush operations. Returns failed stats payloads for retry.
    pub stats_flush_handles: Vec<JoinHandle<Vec<pb::ClientStatsPayload>>>,
}

impl FlushHandles {
    /// Creates a new empty `FlushHandles` instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            trace_flush_handles: Vec::new(),
            log_flush_handles: Vec::new(),
            proxy_flush_handles: Vec::new(),
            stats_flush_handles: Vec::new(),
        }
    }

    /// Returns `true` if any flush operation is still pending (not finished).
    #[must_use]
    pub fn has_pending(&self) -> bool {
        let trace_pending = self.trace_flush_handles.iter().any(|h| !h.is_finished());
        let log_pending = self.log_flush_handles.iter().any(|h| !h.is_finished());
        let proxy_pending = self.proxy_flush_handles.iter().any(|h| !h.is_finished());
        let stats_pending = self.stats_flush_handles.iter().any(|h| !h.is_finished());

        trace_pending || log_pending || proxy_pending || stats_pending
    }

    /// Returns `true` if all handle vectors are empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.trace_flush_handles.is_empty()
            && self.log_flush_handles.is_empty()
            && self.proxy_flush_handles.is_empty()
            && self.stats_flush_handles.is_empty()
    }
}

impl std::fmt::Debug for FlushHandles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlushHandles")
            .field("trace_handles_count", &self.trace_flush_handles.len())
            .field("log_handles_count", &self.log_flush_handles.len())
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
