// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Splits trace payloads that are too large to fit in a single outbound batch.
//!
//! A payload larger than the aggregator's batch cap can never be batched, so it is broken
//! up here before it is queued. Chunks are moved into separate payloads, and a chunk that
//! is still too large has its spans moved into several chunks of the same trace, the same
//! shape tracers produce with partial flushing.

use libdd_trace_protobuf::pb;
use prost::Message;
use prost::encoding::message::encoded_len as field_len;
use prost::encoding::{encoded_len_varint, key_len};
use std::collections::HashMap;
use tracing::warn;

/// Protobuf field number of `TracerPayload.chunks`.
const CHUNKS_TAG: u32 = 6;
/// Protobuf field number of `TraceChunk.spans`.
const SPANS_TAG: u32 = 3;
/// Upper bound on the bytes a length-delimited field adds around its contents
/// (1-byte key for these tags plus a varint length of at most 5 bytes).
const FIELD_OVERHEAD: usize = 6;

/// Splits `payloads` so that each returned payload encodes to at most `max_bytes`.
///
/// Payloads that already fit are returned unchanged. A single span larger than the
/// limit cannot be split further and is dropped with a warning.
#[must_use]
pub fn split_tracer_payloads(
    payloads: Vec<pb::TracerPayload>,
    max_bytes: usize,
) -> Vec<pb::TracerPayload> {
    let mut out = Vec::with_capacity(payloads.len());
    for payload in payloads {
        if payload.encoded_len() <= max_bytes {
            out.push(payload);
        } else {
            split_payload(payload, max_bytes, &mut out);
        }
    }
    out
}

fn split_payload(
    mut payload: pb::TracerPayload,
    max_bytes: usize,
    out: &mut Vec<pb::TracerPayload>,
) {
    let chunks = std::mem::take(&mut payload.chunks);
    let budget = max_bytes.saturating_sub(payload.encoded_len());

    let mut current = Vec::new();
    let mut current_len = 0;
    for chunk in chunks {
        let pieces = if field_len(CHUNKS_TAG, &chunk) <= budget {
            vec![chunk]
        } else {
            split_chunk(chunk, budget)
        };
        for piece in pieces {
            let piece_len = field_len(CHUNKS_TAG, &piece);
            if !current.is_empty() && current_len + piece_len > budget {
                out.push(with_chunks(&payload, std::mem::take(&mut current)));
                current_len = 0;
            }
            current_len += piece_len;
            current.push(piece);
        }
    }
    if !current.is_empty() {
        out.push(with_chunks(&payload, current));
    }
}

/// Splits a chunk into chunks of the same trace whose encoded field size fits in `budget`.
///
/// Trace-level `_dd.p.*` tags (e.g. the upper 64 bits of a 128-bit trace id) are only set on
/// one span of the chunk, so they are copied onto the first span of every piece.
fn split_chunk(mut chunk: pb::TraceChunk, budget: usize) -> Vec<pb::TraceChunk> {
    let spans = std::mem::take(&mut chunk.spans);
    let trace_tags = trace_level_tags(&spans);
    let span_budget = budget.saturating_sub(chunk.encoded_len() + FIELD_OVERHEAD);

    let mut pieces = Vec::new();
    let mut current = Vec::new();
    let mut current_len = 0;
    let mut dropped = 0;
    for span in spans {
        let span_len = field_len(SPANS_TAG, &span);
        // Only a piece's first span gains the trace tags, so this is charged only then.
        let extra = trace_tags_growth(&span, span_len, &trace_tags);
        if span_len + extra > span_budget {
            dropped += 1;
            continue;
        }
        if !current.is_empty() && current_len + span_len > span_budget {
            pieces.push(with_spans(&chunk, std::mem::take(&mut current)));
            current_len = 0;
        }
        current_len += if current.is_empty() {
            span_len + extra
        } else {
            span_len
        };
        current.push(span);
    }
    if !current.is_empty() {
        pieces.push(with_spans(&chunk, current));
    }
    if dropped > 0 {
        warn!("TRACES | Dropped {dropped} span(s) too large to fit in a trace payload");
    }
    for piece in &mut pieces {
        if let Some(first) = piece.spans.first_mut() {
            for (key, value) in &trace_tags {
                first
                    .meta
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    pieces
}

/// Returns how much the encoded `spans` field of `span` (currently `span_len`) grows when
/// the trace tags it lacks are added to its meta.
fn trace_tags_growth(span: &pb::Span, span_len: usize, trace_tags: &[(String, String)]) -> usize {
    let missing: HashMap<String, String> = trace_tags
        .iter()
        .filter(|(k, _)| !span.meta.contains_key(k))
        .cloned()
        .collect();
    if missing.is_empty() {
        return 0;
    }
    // A span holding only the missing tags encodes to exactly the added map entries.
    let added = pb::Span {
        meta: missing,
        ..Default::default()
    }
    .encoded_len();
    let new_len = span.encoded_len() + added;
    key_len(SPANS_TAG) + encoded_len_varint(new_len as u64) + new_len - span_len
}

/// Returns the `_dd.p.*` tags of the chunk's root span, or of the first span that has any.
fn trace_level_tags(spans: &[pb::Span]) -> Vec<(String, String)> {
    let has_trace_tags = |s: &&pb::Span| s.meta.keys().any(|k| k.starts_with("_dd.p."));
    spans
        .iter()
        .filter(has_trace_tags)
        .find(|s| s.parent_id == 0)
        .or_else(|| spans.iter().find(has_trace_tags))
        .map(|s| {
            s.meta
                .iter()
                .filter(|(k, _)| k.starts_with("_dd.p."))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn with_chunks(template: &pb::TracerPayload, chunks: Vec<pb::TraceChunk>) -> pb::TracerPayload {
    pb::TracerPayload {
        chunks,
        ..template.clone()
    }
}

fn with_spans(template: &pb::TraceChunk, spans: Vec<pb::Span>) -> pb::TraceChunk {
    pb::TraceChunk {
        spans,
        ..template.clone()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn span(trace_id: u64, span_id: u64, meta_bytes: usize) -> pb::Span {
        pb::Span {
            trace_id,
            span_id,
            name: "policy.evaluate".to_string(),
            meta: HashMap::from([("payload".to_string(), "x".repeat(meta_bytes))]),
            ..Default::default()
        }
    }

    fn chunk(trace_id: u64, spans: usize, meta_bytes: usize) -> pb::TraceChunk {
        pb::TraceChunk {
            priority: 1,
            origin: "lambda".to_string(),
            spans: (0..spans as u64)
                .map(|i| span(trace_id, i + 1, meta_bytes))
                .collect(),
            tags: HashMap::from([("_dd.p.dm".to_string(), "-0".to_string())]),
            dropped_trace: false,
        }
    }

    fn payload(chunks: Vec<pb::TraceChunk>) -> pb::TracerPayload {
        pb::TracerPayload {
            language_name: "go".to_string(),
            tracer_version: "v1.74.6".to_string(),
            env: "dev".to_string(),
            tags: HashMap::from([("functionname".to_string(), "fn".to_string())]),
            chunks,
            ..Default::default()
        }
    }

    fn span_ids(payloads: &[pb::TracerPayload]) -> Vec<(u64, u64)> {
        let mut ids: Vec<_> = payloads
            .iter()
            .flat_map(|p| p.chunks.iter())
            .flat_map(|c| c.spans.iter())
            .map(|s| (s.trace_id, s.span_id))
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn payloads_under_the_limit_are_unchanged() {
        let input = vec![
            payload(vec![chunk(1, 10, 100)]),
            payload(vec![chunk(2, 5, 100)]),
        ];
        let out = split_tracer_payloads(input.clone(), 1_000_000);
        assert_eq!(out, input);
    }

    #[test]
    fn oversized_payload_is_split_by_chunk() {
        let input = vec![payload((1..=10).map(|t| chunk(t, 10, 1_000)).collect())];
        let limit = input[0].encoded_len() / 3;

        let out = split_tracer_payloads(input.clone(), limit);

        assert!(
            out.len() >= 3,
            "expected at least 3 payloads, got {}",
            out.len()
        );
        for p in &out {
            assert!(p.encoded_len() <= limit, "{} > {limit}", p.encoded_len());
            assert_eq!(p.language_name, "go");
            assert_eq!(p.tags, input[0].tags);
        }
        assert_eq!(span_ids(&out), span_ids(&input));
        // Whole chunks are moved, not split, when each one fits on its own.
        assert_eq!(out.iter().map(|p| p.chunks.len()).sum::<usize>(), 10);
    }

    #[test]
    fn oversized_single_chunk_is_split_by_span() {
        // One trace with many spans, like a single large invocation.
        let input = vec![payload(vec![chunk(7, 2_000, 500)])];
        let limit = input[0].encoded_len() / 5;

        let out = split_tracer_payloads(input.clone(), limit);

        assert!(
            out.len() >= 5,
            "expected at least 5 payloads, got {}",
            out.len()
        );
        for p in &out {
            assert!(p.encoded_len() <= limit, "{} > {limit}", p.encoded_len());
            for c in &p.chunks {
                assert_eq!(c.priority, 1);
                assert_eq!(c.origin, "lambda");
                assert_eq!(c.tags, input[0].chunks[0].tags);
                assert!(c.spans.iter().all(|s| s.trace_id == 7));
            }
        }
        assert_eq!(span_ids(&out), span_ids(&input));
    }

    #[test]
    fn trace_level_tags_are_copied_to_every_split_chunk() {
        let mut trace = chunk(9, 2_000, 500);
        let root = &mut trace.spans[0];
        root.meta
            .insert("_dd.p.tid".to_string(), "66f1e2a300000000".to_string());
        root.meta.insert("_dd.p.dm".to_string(), "-1".to_string());
        let input = vec![payload(vec![trace])];
        let limit = input[0].encoded_len() / 4;

        let out = split_tracer_payloads(input, limit);

        assert!(out.len() >= 4);
        for p in &out {
            assert!(p.encoded_len() <= limit, "{} > {limit}", p.encoded_len());
            for c in &p.chunks {
                let first = &c.spans[0];
                assert_eq!(first.meta["_dd.p.tid"], "66f1e2a300000000");
                assert_eq!(first.meta["_dd.p.dm"], "-1");
            }
        }
    }

    #[test]
    fn span_larger_than_the_limit_is_dropped() {
        let mut big_chunk = chunk(3, 4, 100);
        big_chunk.spans.push(span(3, 99, 50_000));
        let input = vec![payload(vec![big_chunk])];

        let out = split_tracer_payloads(input, 10_000);

        let ids = span_ids(&out);
        assert_eq!(ids, vec![(3, 1), (3, 2), (3, 3), (3, 4)]);
        assert!(out.iter().all(|p| p.encoded_len() <= 10_000));
    }

    #[test]
    fn mixed_payloads_only_split_the_oversized_one() {
        let small = payload(vec![chunk(1, 2, 100)]);
        let large = payload(vec![chunk(2, 1_000, 500)]);
        let limit = large.encoded_len() / 2;

        let out = split_tracer_payloads(vec![small.clone(), large.clone()], limit);

        assert_eq!(out[0], small);
        assert!(out.len() >= 3);
        assert!(out.iter().all(|p| p.encoded_len() <= limit));
        assert_eq!(span_ids(&out), span_ids(&[small, large]));
    }

    #[test]
    fn near_limit_span_with_trace_tags_is_kept() {
        let mut root = span(5, 1, 0);
        root.meta
            .insert("_dd.p.tid".to_string(), "66f1e2a300000000".to_string());
        root.meta.insert("_dd.p.dm".to_string(), "-1".to_string());
        root.meta.insert("payload".to_string(), "x".repeat(5_000));
        let mut trace = chunk(5, 0, 0);
        let empty_chunk_len = trace.encoded_len();
        trace.spans.push(root);
        trace.spans.extend((2..=5).map(|i| span(5, i, 100)));
        let input = payload(vec![trace]);
        let empty_payload_len = {
            let mut p = input.clone();
            p.chunks.clear();
            p.encoded_len()
        };
        // The root span leaves only a few bytes of the space available to a single span. It
        // already carries the trace tags, so it gains nothing when they are propagated.
        let root_len = field_len(SPANS_TAG, &input.chunks[0].spans[0]);
        let limit = empty_payload_len + empty_chunk_len + FIELD_OVERHEAD + root_len + 10;
        assert!(input.encoded_len() > limit);

        let out = split_tracer_payloads(vec![input.clone()], limit);

        assert!(span_ids(&out).contains(&(5, 1)), "root span was dropped");
        assert_eq!(span_ids(&out), span_ids(&[input]));
        assert!(out.iter().all(|p| p.encoded_len() <= limit));
    }
}
