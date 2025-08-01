use std::io;

use bytes::{Buf, Bytes};
use zstd::Decoder;

/// The ruleset is embedded as a ZSTD-compressed JSON file (it takes ~10 times less space this way).
const DEFAULT_RECOMMENDED_RULES: &[u8] = include_bytes!("default-recommended-ruleset.json.zst");

/// Returns a reader for the recommended default ruleset's JSON document.
pub(super) fn default_recommended_ruleset() -> impl io::Read {
    Decoder::new(Bytes::from(DEFAULT_RECOMMENDED_RULES).reader())
        .expect("failed to create ruleset reader")
}
