/// The maximum number of bytes that a metric name will be.
pub const MAX_METRIC_NAME_BYTES: usize = 128;

/// The maximum values that a `Metric` may hold.
pub const MAX_VALUES: usize = 8;

/// The maximum number of bytes that a value string represenation will be.
pub const MAX_VALUE_BYTES: usize = 32;

/// The maximum tags that a `Metric` may hold.
pub const MAX_TAGS: usize = 32;

/// The maximum number of bytes that a tag key will be.
pub const MAX_TAG_KEY_BYTES: usize = 64;

/// The maximum number of bytes that a tag value will be.
pub const MAX_TAG_VALUE_BYTES: usize = 64;

/// The maximum number of bytes that a container ID will be.
pub const MAX_CONTAINER_ID_BYTES: usize = 64;

pub const CONTEXTS: usize = 1024;

pub static MAX_CONTEXTS: usize = 65_536; // 2**16, arbitrary
