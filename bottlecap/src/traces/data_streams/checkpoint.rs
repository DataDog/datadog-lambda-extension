//! Consume-side DSM checkpoint computation.
//!
//! This is the extension-only subset of `dd-trace-js`'s `setCheckpoint`: we only
//! ever produce a single inbound (`direction:in`) checkpoint continuing from an
//! extracted parent context. The tracer's in-process `closestOppositeDirection`
//! loop handling does not apply, because the extension never observes the
//! produce side of a pathway.

use crate::traces::data_streams::context::DsmContext;
use crate::traces::data_streams::pathway::compute_pathway_hash;

/// Parent hash used when there is no inbound context (pathway entry point).
pub const ENTRY_PARENT_HASH: [u8; 8] = [0; 8];

/// A computed consume checkpoint, ready to be folded into a stats bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// This checkpoint's pathway hash.
    pub hash: [u8; 8],
    /// The parent pathway hash this checkpoint continues from.
    pub parent_hash: [u8; 8],
    /// Sorted edge tags (direction tag first, as supplied by the caller).
    pub edge_tags: Vec<String>,
    /// Time the producer-to-consumer edge took, in nanoseconds.
    pub edge_latency_ns: u64,
    /// Total pathway latency from origin to here, in nanoseconds.
    pub pathway_latency_ns: u64,
    /// Wall-clock time of this checkpoint, in nanoseconds (used for bucketing).
    pub current_ts_ns: u64,
}

/// Compute an inbound (`direction:in`) consume checkpoint.
///
/// * `edge_tags` must contain `direction:in` and the source-specific tags.
/// * `ctx` is the extracted inbound DSM context, if any.
/// * `now_ns` is the current wall-clock time in nanoseconds.
/// * `propagation_hash` is the optional process/container-tag hash.
#[must_use]
pub fn compute_consume_checkpoint(
    service: &str,
    env: &str,
    edge_tags: &[String],
    ctx: Option<&DsmContext>,
    now_ns: u64,
    propagation_hash: Option<u64>,
) -> Checkpoint {
    let (parent_hash, pathway_start_ns, edge_start_ns) = match ctx {
        Some(ctx) => (ctx.hash, ctx.pathway_start_ns, ctx.edge_start_ns),
        None => (ENTRY_PARENT_HASH, now_ns, now_ns),
    };

    let hash = compute_pathway_hash(service, env, edge_tags, parent_hash, propagation_hash);

    // Saturating: a clock skew where the stored start is in the future yields 0
    // latency rather than a wildly large wrapped value.
    let edge_latency_ns = now_ns.saturating_sub(edge_start_ns);
    let pathway_latency_ns = now_ns.saturating_sub(pathway_start_ns);

    Checkpoint {
        hash,
        parent_hash,
        edge_tags: edge_tags.to_vec(),
        edge_latency_ns,
        pathway_latency_ns,
        current_ts_ns: now_ns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> Vec<String> {
        vec![
            "direction:in".to_string(),
            "topic:my-topic".to_string(),
            "type:sqs".to_string(),
        ]
    }

    #[test]
    fn continues_from_extracted_context() {
        let ctx = DsmContext {
            hash: [1, 2, 3, 4, 5, 6, 7, 8],
            pathway_start_ns: 1_000_000_000,
            edge_start_ns: 1_500_000_000,
        };
        let now = 2_000_000_000;

        let cp = compute_consume_checkpoint("svc", "env", &tags(), Some(&ctx), now, None);

        assert_eq!(cp.parent_hash, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(cp.edge_latency_ns, 500_000_000); // now - edge_start
        assert_eq!(cp.pathway_latency_ns, 1_000_000_000); // now - pathway_start
        assert_eq!(cp.current_ts_ns, now);
        // Hash must match a direct pathway-hash computation with the parent.
        assert_eq!(
            cp.hash,
            compute_pathway_hash("svc", "env", &tags(), ctx.hash, None)
        );
    }

    #[test]
    fn entry_point_when_no_context() {
        let now = 2_000_000_000;
        let cp = compute_consume_checkpoint("svc", "env", &tags(), None, now, None);

        assert_eq!(cp.parent_hash, ENTRY_PARENT_HASH);
        assert_eq!(cp.edge_latency_ns, 0);
        assert_eq!(cp.pathway_latency_ns, 0);
    }

    #[test]
    fn clock_skew_saturates_to_zero() {
        let ctx = DsmContext {
            hash: [0; 8],
            pathway_start_ns: 5_000_000_000, // in the future relative to now
            edge_start_ns: 5_000_000_000,
        };
        let cp = compute_consume_checkpoint("svc", "env", &tags(), Some(&ctx), 1_000_000_000, None);
        assert_eq!(cp.edge_latency_ns, 0);
        assert_eq!(cp.pathway_latency_ns, 0);
    }
}
