//! Optional DSM propagation hash.
//!
//! Used when process-tag propagation is enabled. The input is the serialized
//! process tags plus the container-tags hash returned by the agent.
//!
//! NOTE: `dd-trace-js` comments describe this as "FNV-1a", but the code performs
//! multiply-then-XOR, i.e. FNV-1 (not FNV-1a). We match the implementation, not
//! the comment, for compatibility.

const FNV1_64_OFFSET_BASIS: u64 = 0xCBF2_9CE4_8422_2325;
const FNV1_64_PRIME: u64 = 0x0000_0100_0000_01B3;

/// Compute the FNV-1 (64-bit) hash over `bytes`, matching `dd-trace-js`.
#[must_use]
pub fn fnv1_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1_64_OFFSET_BASIS;
    for &byte in bytes {
        hash = hash.wrapping_mul(FNV1_64_PRIME);
        hash ^= u64::from(byte);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_is_offset_basis() {
        assert_eq!(fnv1_64(b""), FNV1_64_OFFSET_BASIS);
    }

    #[test]
    fn is_deterministic() {
        assert_eq!(fnv1_64(b"process-tags:foo"), fnv1_64(b"process-tags:foo"));
    }

    #[test]
    fn differs_by_input() {
        assert_ne!(fnv1_64(b"foo"), fnv1_64(b"bar"));
    }

    #[test]
    fn applies_multiply_before_xor() {
        // FNV-1 (multiply-then-XOR) differs from FNV-1a (XOR-then-multiply).
        // Verify the first step explicitly for a single byte.
        let expected = FNV1_64_OFFSET_BASIS
            .wrapping_mul(FNV1_64_PRIME)
            ^ u64::from(b'a');
        assert_eq!(fnv1_64(b"a"), expected);
    }
}
