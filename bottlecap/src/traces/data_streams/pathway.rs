//! DSM pathway hash computation, byte-for-byte compatible with `dd-trace-js`.
//!
//! See `docs`/design notes: the algorithm intentionally preserves a quirk where
//! the 16-byte `current_hash || parent_hash` buffer is converted to a (lossy)
//! UTF-8 string *before* the final SHA-256, rather than hashing the raw bytes.
//! Do not "simplify" this to `sha256(&combined)` — it would break compatibility
//! with pathways produced by the tracers.

use sha2::{Digest, Sha256};
use std::fmt::Write as _;

const MANUAL_CHECKPOINT_TAG: &str = "manual_checkpoint:true";

/// First 8 bytes of `SHA-256(bytes)`.
fn sha256_first8(bytes: &[u8]) -> [u8; 8] {
    let digest = Sha256::digest(bytes);
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// Compute a DSM pathway hash for a checkpoint.
///
/// * `service` / `env` — local service identity.
/// * `edge_tags` — checkpoint edge tags (e.g. `direction:in`, `type:sqs`).
///   Sorted and de-`manual_checkpoint`-ed before hashing.
/// * `parent_hash` — raw 8-byte parent pathway hash (zero bytes if no parent).
/// * `propagation_hash` — optional process/container-tag propagation hash.
#[must_use]
pub fn compute_pathway_hash(
    service: &str,
    env: &str,
    edge_tags: &[String],
    parent_hash: [u8; 8],
    propagation_hash: Option<u64>,
) -> [u8; 8] {
    let mut tags = edge_tags.to_vec();
    tags.sort_unstable();

    let joined_tags = tags
        .iter()
        .filter(|tag| tag.as_str() != MANUAL_CHECKPOINT_TAG)
        .map(String::as_str)
        .collect::<String>();

    let mut base = format!("{service}{env}{joined_tags}");
    if let Some(hash) = propagation_hash {
        // Appended as ":" + lowercase hex with no "0x" prefix and no leading zeros,
        // matching JS `Number.prototype.toString(16)`.
        write!(&mut base, ":{hash:x}").expect("writing to String cannot fail");
    }

    let current_hash = sha256_first8(base.as_bytes());

    let mut combined = [0u8; 16];
    combined[..8].copy_from_slice(&current_hash);
    combined[8..].copy_from_slice(&parent_hash);

    // Compatibility-critical: lossy UTF-8 round-trip before the final hash.
    let combined_string = String::from_utf8_lossy(&combined);
    sha256_first8(combined_string.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_string()).collect()
    }

    /// Pinned `dd-trace-js` fixture.
    #[test]
    fn matches_pinned_pathway_hash() {
        let hash = compute_pathway_hash(
            "test-service",
            "test-env",
            &tags(&["direction:in", "group:group1", "topic:topic1", "type:kafka"]),
            [0u8; 8],
            None,
        );
        assert_eq!(hex::encode(hash), "67b0b35e65c0acfa");
    }

    #[test]
    fn tag_order_does_not_change_hash() {
        let sorted = compute_pathway_hash(
            "test-service",
            "test-env",
            &tags(&["direction:in", "group:group1", "topic:topic1", "type:kafka"]),
            [0u8; 8],
            None,
        );
        let shuffled = compute_pathway_hash(
            "test-service",
            "test-env",
            &tags(&["type:kafka", "topic:topic1", "direction:in", "group:group1"]),
            [0u8; 8],
            None,
        );
        assert_eq!(sorted, shuffled);
    }

    #[test]
    fn manual_checkpoint_tag_is_excluded() {
        let without = compute_pathway_hash(
            "svc",
            "env",
            &tags(&["direction:in"]),
            [0u8; 8],
            None,
        );
        let with = compute_pathway_hash(
            "svc",
            "env",
            &tags(&["direction:in", "manual_checkpoint:true"]),
            [0u8; 8],
            None,
        );
        assert_eq!(without, with);
    }

    #[test]
    fn parent_hash_changes_result() {
        let a = compute_pathway_hash("svc", "env", &tags(&["direction:in"]), [0u8; 8], None);
        let b = compute_pathway_hash(
            "svc",
            "env",
            &tags(&["direction:in"]),
            [1, 2, 3, 4, 5, 6, 7, 8],
            None,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn propagation_hash_changes_result() {
        let absent = compute_pathway_hash("svc", "env", &tags(&["direction:in"]), [0u8; 8], None);
        let present = compute_pathway_hash(
            "svc",
            "env",
            &tags(&["direction:in"]),
            [0u8; 8],
            Some(0x1234_5678_9abc_def0),
        );
        let present_repeat = compute_pathway_hash(
            "svc",
            "env",
            &tags(&["direction:in"]),
            [0u8; 8],
            Some(0x1234_5678_9abc_def0),
        );
        let different = compute_pathway_hash(
            "svc",
            "env",
            &tags(&["direction:in"]),
            [0u8; 8],
            Some(0x0fed_cba9_8765_4321),
        );

        assert_ne!(absent, present);
        assert_eq!(present, present_repeat);
        assert_ne!(present, different);
    }
}
