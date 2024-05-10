/// Maximum content size per payload uncompressed in bytes,
/// that the Datadog Logs API accepts.
pub const MAX_CONTENT_SIZE_BYTES: usize = 5 * 1_024 * 1_024;

/// Maximum size in bytes of a single log entry, before it
/// gets truncated.
pub const MAX_LOG_SIZE_BYTES: usize = 1_024 * 1_024;

/// Maximum logs array size accepted.
pub const MAX_BATCH_ENTRIES_SIZE: usize = 1000;
