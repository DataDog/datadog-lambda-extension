/// The maximum tags that a `Metric` may hold.
pub const MAX_TAGS: usize = 32;

pub const CONTEXTS: usize = 1024;

pub static MAX_CONTEXTS: usize = 65_536; // 2**16, arbitrary

const MB: usize = 1_024 * 1_024;

pub(crate) const MAX_ENTRIES_NUMBER_SINGLE_METRIC: usize = 1_000;

pub(crate) const MAX_CONTENT_SIZE_BYTES_SINGLE_METRIC: usize = 5 * MB;

pub(crate) const MAX_ENTRIES_NUMBER_SKETCH_METRIC: usize = 1_000;

pub(crate) const MAX_CONTENT_SIZE_SKETCH_METRIC: usize = 62 * MB;
