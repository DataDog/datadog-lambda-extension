use sha2::{Digest, Sha256};

const SPAN_POINTER_HASH_LENGTH: usize = 32;

#[derive(Clone)]
pub struct SpanPointer {
    pub hash: String,
    pub kind: String,
}

/// Returns the first 32 characters of the SHA-256 hash of the components joined by a '|'.
/// Used by span pointers to uniquely & deterministically identify an S3 or DynamoDB stream.
/// https://github.com/DataDog/dd-span-pointer-rules/blob/main/README.md#General%20Hashing%20Rules
#[must_use]
pub fn generate_span_pointer_hash(components: &[&str]) -> String {
    let data_to_hash = components.join("|");
    let mut hasher = Sha256::new();
    hasher.update(data_to_hash.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)[..SPAN_POINTER_HASH_LENGTH].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_span_pointer_hash() {
        let test_cases = vec![
            (
                "basic values",
                vec!["some-bucket", "some-key.data", "ab12ef34"],
                "e721375466d4116ab551213fdea08413",
            ),
            (
                "non-ascii key",
                vec!["some-bucket", "some-key.你好", "ab12ef34"],
                "d1333a04b9928ab462b5c6cadfa401f4",
            ),
            (
                "multipart-upload",
                vec!["some-bucket", "some-key.data", "ab12ef34-5"],
                "2b90dffc37ebc7bc610152c3dc72af9f",
            ),
        ];

        for (name, components, expected_hash) in test_cases {
            let actual_hash = generate_span_pointer_hash(&components);
            assert_eq!(actual_hash, expected_hash, "Test case: {name}");
        }
    }
}
