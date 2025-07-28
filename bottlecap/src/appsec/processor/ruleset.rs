use std::io;

use bytes::{Buf, Bytes};
use flate2::read::GzDecoder;

/// The ruleset is embedded as a gzipped JSON file (it takes ~10 times less space this way).
const DEFAULT_RECOMMENDED_RULES: &[u8] = include_bytes!("default-recommended-ruleset.json.gz");

/// Returns a reader for the recommended default ruleset's JSON document.
pub(super) fn default_recommended_ruleset() -> impl io::Read {
    GzDecoder::new(Bytes::from(DEFAULT_RECOMMENDED_RULES).reader())
}
