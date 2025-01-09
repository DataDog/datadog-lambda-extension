// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const SPAN_POINTER_HASH_LENGTH: usize = 32;

#[derive(Clone)]
pub struct SpanPointer {
    pub hash: String,
    pub kind: String,
}

/// Returns the first 32 characters of the SHA-256 hash of the components joined by a '|'.
/// Used by span pointers to uniquely & deterministically identify an `S3` or `DynamoDB` stream.
/// <https://github.com/DataDog/dd-span-pointer-rules/blob/main/README.md#General%20Hashing%20Rules>
#[must_use]
pub fn generate_span_pointer_hash(components: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(components.join("|").as_bytes());
    let result = hasher.finalize();
    hex::encode(result)[..SPAN_POINTER_HASH_LENGTH].to_string()
}

pub fn attach_span_pointers_to_meta<S: ::std::hash::BuildHasher>(
    meta: &mut HashMap<String, String, S>,
    span_pointers: &[SpanPointer],
) {
    if span_pointers.is_empty() {
        return;
    }

    let span_links: Vec<Value> = span_pointers
        .iter()
        .map(|sp| {
            json!({
                "attributes": {
                    "link.kind": "span-pointer",
                    "ptr.dir": "u",
                    "ptr.hash": sp.hash,
                    "ptr.kind": sp.kind,
                },
                "span_id": "0",
                "trace_id": "0"
            })
        })
        .collect();

    if let Ok(span_links_json) = serde_json::to_string(&span_links) {
        meta.insert("_dd.span_links".to_string(), span_links_json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[derive(Debug, Default)]
    struct TestSpan {
        pub meta: HashMap<String, String>,
    }

    struct SpanPointerTestCase {
        test_name: &'static str,
        span_pointers: Option<Vec<SpanPointer>>,
        expected_links: Option<serde_json::Value>,
    }

    #[test]
    fn test_attach_span_pointers_to_span() {
        let test_cases = vec![
            SpanPointerTestCase {
                test_name: "adds span links to span",
                span_pointers: Some(vec![
                    SpanPointer {
                        hash: "hash1".to_string(),
                        kind: "test.kind1".to_string(),
                    },
                    SpanPointer {
                        hash: "hash2".to_string(),
                        kind: "test.kind2".to_string(),
                    },
                ]),
                expected_links: Some(json!([
                    {
                        "attributes": {
                            "link.kind": "span-pointer",
                            "ptr.dir": "u",
                            "ptr.hash": "hash1",
                            "ptr.kind": "test.kind1"
                        },
                        "span_id": "0",
                        "trace_id": "0"
                    },
                    {
                        "attributes": {
                            "link.kind": "span-pointer",
                            "ptr.dir": "u",
                            "ptr.hash": "hash2",
                            "ptr.kind": "test.kind2"
                        },
                        "span_id": "0",
                        "trace_id": "0"
                    }
                ])),
            },
            SpanPointerTestCase {
                test_name: "handles empty span pointers",
                span_pointers: Some(vec![]),
                expected_links: None,
            },
        ];

        for case in test_cases {
            let mut test_span = TestSpan {
                meta: HashMap::new(),
            };

            if let Some(ref pointers) = case.span_pointers {
                attach_span_pointers_to_meta(&mut test_span.meta, pointers);
            }

            match case.expected_links {
                Some(expected) => {
                    let span_links = test_span.meta.get("_dd.span_links").unwrap_or_else(|| {
                        panic!(
                            "[{}] _dd.span_links should be present in span meta",
                            case.test_name
                        )
                    });
                    let actual_links: serde_json::Value =
                        serde_json::from_str(span_links).expect("Should be valid JSON");
                    assert_eq!(
                        actual_links, expected,
                        "Failed test case: {}",
                        case.test_name
                    );
                }
                None => {
                    assert!(
                        !test_span.meta.contains_key("_dd.span_links"),
                        "Failed test case: {}",
                        case.test_name
                    );
                }
            }
        }
    }

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
