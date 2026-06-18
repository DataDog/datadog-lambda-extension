//! Decoding of inbound Data Streams Monitoring (DSM) pathway context.
//!
//! The wire format (after base64 decoding) is:
//!   1. first 8 bytes: raw pathway hash
//!   2. zigzag-encoded signed varint (protobuf `sint64`): `pathwayStartMs`
//!   3. zigzag-encoded signed varint (protobuf `sint64`): `edgeStartMs`
//!
//! NOTE: an earlier design note described these as plain unsigned varints. The
//! `dd-trace-js` tracer actually zigzag-encodes them (a positive `n` is stored
//! as `2n`), so they must be zigzag-decoded to recover the millisecond value.
//!
//! Both timestamps are stored in milliseconds and converted to nanoseconds by
//! multiplying by `1_000_000`, matching `dd-trace-js`.
//!
//! All decoding fails closed: malformed payloads return `None` and are treated
//! as "no parent DSM context".

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Carrier key (preferred) holding the base64-encoded DSM pathway context.
pub const DD_PATHWAY_CTX_BASE64_KEY: &str = "dd-pathway-ctx-base64";
/// Legacy carrier key holding the raw (binary) DSM pathway context.
pub const DD_PATHWAY_CTX_KEY: &str = "dd-pathway-ctx";

const MS_TO_NS: u64 = 1_000_000;

/// An inbound DSM pathway context extracted from a carrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsmContext {
    /// Raw 8-byte parent pathway hash (opaque; do not reinterpret for hashing).
    pub hash: [u8; 8],
    /// Pathway start time in nanoseconds.
    pub pathway_start_ns: u64,
    /// Edge start time in nanoseconds.
    pub edge_start_ns: u64,
}

impl DsmContext {
    /// Decode a DSM context from a base64-encoded `dd-pathway-ctx-base64` value.
    #[must_use]
    pub fn from_base64(input: &str) -> Option<Self> {
        let bytes = STANDARD.decode(input).ok()?;
        Self::from_bytes(&bytes)
    }

    /// Decode a DSM context from its raw binary representation.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }

        let mut hash = [0u8; 8];
        hash.copy_from_slice(&bytes[..8]);

        let (pathway_start_ms, rest) = decode_zigzag_varint(&bytes[8..])?;
        let (edge_start_ms, _) = decode_zigzag_varint(rest)?;

        Some(Self {
            hash,
            pathway_start_ns: ms_to_ns(pathway_start_ms)?,
            edge_start_ns: ms_to_ns(edge_start_ms)?,
        })
    }
}

/// Convert a (signed) millisecond timestamp to nanoseconds. Negative values are
/// rejected — DSM timestamps are always positive wall-clock times.
fn ms_to_ns(ms: i64) -> Option<u64> {
    u64::try_from(ms).ok()?.checked_mul(MS_TO_NS)
}

/// Decode a zigzag-encoded signed varint (protobuf `sint64`).
fn decode_zigzag_varint(bytes: &[u8]) -> Option<(i64, &[u8])> {
    let (raw, rest) = decode_uvarint(bytes)?;
    // Zigzag decode: (raw >> 1) ^ -(raw & 1).
    #[allow(clippy::cast_possible_wrap)]
    let decoded = ((raw >> 1) as i64) ^ -((raw & 1) as i64);
    Some((decoded, rest))
}

/// Decode an unsigned LEB128 varint, returning the value and the remaining bytes.
///
/// Returns `None` if the input is truncated or the varint overflows `u64`.
fn decode_uvarint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;

    for (idx, &byte) in bytes.iter().enumerate() {
        // A u64 holds at most 10 varint groups (last group contributes 1 bit).
        if shift >= 64 {
            return None;
        }
        let payload = u64::from(byte & 0x7f);
        result |= payload.checked_shl(shift)?;

        if byte & 0x80 == 0 {
            return Some((result, &bytes[idx + 1..]));
        }
        shift += 7;
    }

    // Ran out of bytes before the terminating group.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned `dd-trace-js` fixture.
    const FIXTURE_B64: &str = "Z7CzXmXArPrE58Cfj2LI2cOfj2I=";

    #[test]
    fn decodes_pinned_base64_fixture() {
        let ctx = DsmContext::from_base64(FIXTURE_B64).expect("should decode");

        assert_eq!(hex::encode(ctx.hash), "67b0b35e65c0acfa");
        assert_eq!(ctx.pathway_start_ns, 1_685_673_482_722_000_000);
        assert_eq!(ctx.edge_start_ns, 1_685_673_506_404_000_000);
    }

    #[test]
    fn rejects_short_context() {
        assert!(DsmContext::from_bytes(&[0u8; 7]).is_none());
    }

    #[test]
    fn rejects_missing_varints() {
        // 8 hash bytes but no varints follows.
        assert!(DsmContext::from_bytes(&[0u8; 8]).is_none());
    }

    #[test]
    fn rejects_truncated_varint() {
        // Hash + a varint with continuation bit set but no following byte.
        let mut bytes = vec![0u8; 8];
        bytes.push(0x80);
        assert!(DsmContext::from_bytes(&bytes).is_none());
    }

    #[test]
    fn rejects_invalid_base64() {
        assert!(DsmContext::from_base64("not valid base64!!!").is_none());
    }

    #[test]
    fn uvarint_single_byte() {
        let (value, rest) = decode_uvarint(&[0x01]).expect("decode");
        assert_eq!(value, 1);
        assert!(rest.is_empty());
    }

    #[test]
    fn zigzag_decodes_positive() {
        // 1685673482722 zigzag-encoded is 2 * 1685673482722 = 3371346965444.
        // 3371346965444 in LEB128: encode and decode round-trip via the public API
        // is covered by the pinned fixture; here we check the helper directly.
        let (value, _) = decode_zigzag_varint(&[0xac, 0x02]).expect("decode");
        // raw uvarint 300 -> zigzag -> 150
        assert_eq!(value, 150);
    }

    #[test]
    fn uvarint_multi_byte() {
        // 300 = 0xAC 0x02 in LEB128.
        let (value, rest) = decode_uvarint(&[0xac, 0x02, 0xff]).expect("decode");
        assert_eq!(value, 300);
        assert_eq!(rest, &[0xff]);
    }
}
