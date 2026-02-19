use base64::{DecodeError, Engine, engine::general_purpose};
use libdd_trace_protobuf::pb::Span;
use rand::{Rng, RngCore, rngs::OsRng};
use std::collections::HashMap;

use crate::tags::lambda::tags::{INIT_TYPE, SNAP_START_VALUE};
use serde_json::Value;
use tracing::debug;

pub mod context;
pub mod processor;
pub mod processor_service;
pub mod span_inferrer;
pub mod triggers;

const MAX_TAG_CHARS: usize = 4096;
const REDACTABLE_KEYS: [&str; 8] = [
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "authorization",
    "x-authorization",
    "api_key",
];

pub fn base64_to_string(base64_string: &str) -> Result<String, DecodeError> {
    match general_purpose::STANDARD.decode(base64_string) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).to_string()),
        Err(e) => Err(e),
    }
}

fn create_empty_span(name: String, resource: &str, service: &str) -> Span {
    let mut span = Span {
        name,
        resource: resource.to_string(),
        service: service.to_string(),
        r#type: String::from("serverless"),
        ..Default::default()
    };

    // Add span.kind to the span to enable other server based features for serverless
    span.meta
        .insert("span.kind".to_string(), "server".to_string());

    span
}

#[must_use]
pub fn generate_span_id() -> u64 {
    if std::env::var(INIT_TYPE).is_ok_and(|it| it == SNAP_START_VALUE) {
        return OsRng.next_u64();
    }

    let mut rng = rand::thread_rng();
    rng.r#gen()
}

fn redact_value(key: &str, value: String) -> String {
    let split_key = key.split('.').next_back().unwrap_or_default();
    if REDACTABLE_KEYS.contains(&split_key) {
        String::from("redacted")
    } else {
        value
    }
}

pub fn get_metadata_from_value(
    key: &str,
    value: &Value,
    depth: u32,
    max_depth: u32,
) -> HashMap<String, String> {
    let mut metadata = HashMap::new();

    // Null scenario
    if value.is_null() {
        metadata.insert(key.to_string(), value.to_string());
        return metadata;
    }

    // Check max depth
    if depth >= max_depth {
        match serde_json::to_string(value) {
            Ok(s) => {
                let truncated = s.chars().take(MAX_TAG_CHARS).collect::<String>();
                metadata.insert(key.to_string(), truncated);
                return metadata;
            }
            Err(e) => {
                debug!("Unable to serialize value for tagging {e}");
                return metadata;
            }
        }
    }

    let new_depth = depth + 1;
    match value {
        // Handle string case
        Value::String(s) => {
            if let Ok(p) = serde_json::from_str::<Value>(s) {
                metadata.extend(get_metadata_from_value(key, &p, new_depth, max_depth));
            } else {
                let truncated = s.chars().take(MAX_TAG_CHARS).collect::<String>();
                metadata.insert(key.to_string(), redact_value(key, truncated));
            }
        }

        // Handle number case
        Value::Number(n) => {
            metadata.insert(key.to_string(), n.to_string());
        }

        // Handle boolean case
        Value::Bool(b) => {
            metadata.insert(key.to_string(), b.to_string());
        }

        // Handle object case
        Value::Object(map) => {
            for (k, v) in map {
                let new_key = format!("{key}.{k}");
                metadata.extend(get_metadata_from_value(&new_key, v, new_depth, max_depth));
            }
        }

        Value::Array(a) => {
            if a.is_empty() {
                metadata.insert(key.to_string(), "[]".to_string());
                return metadata;
            }

            for (i, v) in a.iter().enumerate() {
                let new_key = format!("{key}.{i}");
                metadata.extend(get_metadata_from_value(&new_key, v, new_depth, max_depth));
            }
        }
        Value::Null => {}
    }

    metadata
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    #[test]
    fn test_get_metadata_from_value_simple_tagging() {
        let value = json!({ "request": { "simple": "value" } });

        let metadata = get_metadata_from_value("payload", &value, 0, 10);

        let expected = HashMap::from([("payload.request.simple".to_string(), "value".to_string())]);

        assert_eq!(metadata, expected);
    }

    #[test]
    fn test_get_metadata_from_value_complex_object() {
        let value = json!({
            "request": {
                "simple": "value",
                "obj": {
                    "arr": ["a", "b", "c"],
                    "boolean": true,
                    "nested": {
                        "value": "nested_value"
                    }
                },
                "empty": null,
                "number": 1,
                "boolean": true,
            }
        });

        let metadata = get_metadata_from_value("payload", &value, 0, 10);

        let expected = HashMap::from([
            ("payload.request.simple".to_string(), "value".to_string()),
            ("payload.request.obj.arr.0".to_string(), "a".to_string()),
            ("payload.request.obj.arr.1".to_string(), "b".to_string()),
            ("payload.request.obj.arr.2".to_string(), "c".to_string()),
            (
                "payload.request.obj.boolean".to_string(),
                "true".to_string(),
            ),
            (
                "payload.request.obj.nested.value".to_string(),
                "nested_value".to_string(),
            ),
            ("payload.request.empty".to_string(), "null".to_string()),
            ("payload.request.number".to_string(), "1".to_string()),
            ("payload.request.boolean".to_string(), "true".to_string()),
        ]);

        assert_eq!(metadata, expected);
    }

    #[test]
    fn test_get_metadata_from_value_array_of_objects() {
        let value = json!({
            "request": [
                { "simple": "value" },
                { "simple": "value" },
                { "simple": "value" },
            ]
        });

        let metadata = get_metadata_from_value("payload", &value, 0, 10);

        let expected = HashMap::from([
            ("payload.request.0.simple".to_string(), "value".to_string()),
            ("payload.request.1.simple".to_string(), "value".to_string()),
            ("payload.request.2.simple".to_string(), "value".to_string()),
        ]);

        assert_eq!(metadata, expected);
    }

    #[test]
    fn test_get_metadata_from_value_reach_max_depth() {
        let value = json!({
            "hello": "world",
            "empty": null,
            "level1": {
                "obj": {
                    "level3": 3
                },
                "arr": [null, true, "great", { "l3": "v3" }],
                "boolean": true,
                "number": 2,
                "empty": null,
                "empty_obj": {},
                "empty_arr": []
            },
            "arr": [{ "a": "b" }, { "c": "d" }]
        });

        let metadata = get_metadata_from_value("payload", &value, 0, 2);

        let expected = HashMap::from([
            ("payload.hello".to_string(), "world".to_string()),
            ("payload.empty".to_string(), "null".to_string()),
            (
                "payload.level1.obj".to_string(),
                "{\"level3\":3}".to_string(),
            ),
            (
                "payload.level1.arr".to_string(),
                "[null,true,\"great\",{\"l3\":\"v3\"}]".to_string(),
            ),
            ("payload.level1.boolean".to_string(), "true".to_string()),
            ("payload.level1.number".to_string(), "2".to_string()),
            ("payload.level1.empty".to_string(), "null".to_string()),
            ("payload.level1.empty_obj".to_string(), "{}".to_string()),
            ("payload.level1.empty_arr".to_string(), "[]".to_string()),
            ("payload.arr.0".to_string(), "{\"a\":\"b\"}".to_string()),
            ("payload.arr.1".to_string(), "{\"c\":\"d\"}".to_string()),
        ]);

        assert_eq!(metadata, expected);
    }

    #[test]
    fn test_get_metadata_from_value_tag_redacts_key() {
        let value = json!({
            "request": {
                "headers": {
                    "authorization": "secret token",
                }
            }
        });

        let metadata = get_metadata_from_value("payload", &value, 0, 10);

        let expected = HashMap::from([(
            "payload.request.headers.authorization".to_string(),
            "redacted".to_string(),
        )]);

        assert_eq!(metadata, expected);
    }
}
