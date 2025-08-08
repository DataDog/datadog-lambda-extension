use std::io;

use zstd::Decoder;

/// The ruleset is embedded as a ZSTD-compressed JSON file (it takes ~10 times less space this way).
const DEFAULT_RECOMMENDED_RULES: &[u8] = include_bytes!("default-recommended-ruleset.json.zst");

/// Returns a reader for the recommended default ruleset's JSON document.
pub(super) fn default_recommended_ruleset() -> impl io::Read {
    Decoder::new(DEFAULT_RECOMMENDED_RULES).expect("failed to create ruleset reader")
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_default_recommended_ruleset() {
        let reader = super::default_recommended_ruleset();
        serde_json::from_reader::<_, serde_json::Value>(reader).expect("failed to parse ruleset");
    }
}
