use base64::{engine::general_purpose, DecodeError, Engine};
use datadog_trace_protobuf::pb::Span;
use serde_json::Value;
use tracing::debug;

pub mod context;
pub mod processor;
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

pub fn tag_span_from_value(span: &mut Span, key: &str, value: &Value, depth: u32, max_depth: u32) {
    // Null scenario
    if value.is_null() {
        span.meta.insert(key.to_string(), value.to_string());
        return;
    }

    // Check max depth
    if depth >= max_depth {
        match serde_json::to_string(value) {
            Ok(s) => {
                let truncated = s.chars().take(MAX_TAG_CHARS).collect::<String>();
                span.meta.insert(key.to_string(), truncated);
                return;
            }
            Err(e) => {
                debug!("Unable to serialize value for tagging {e}");
                return;
            }
        }
    }

    let new_depth = depth + 1;
    match value {
        // Handle string case
        Value::String(s) => {
            if let Ok(p) = serde_json::from_str::<Value>(s) {
                tag_span_from_value(span, key, &p, new_depth, max_depth);
            } else {
                let truncated = s.chars().take(MAX_TAG_CHARS).collect::<String>();
                span.meta
                    .insert(key.to_string(), redact_value(key, truncated));
            }
        }

        // Handle number case
        Value::Number(n) => {
            span.meta.insert(key.to_string(), n.to_string());
        }

        // Handle boolean case
        Value::Bool(b) => {
            span.meta.insert(key.to_string(), b.to_string());
        }

        // Handle object case
        Value::Object(map) => {
            for (k, v) in map {
                let new_key = format!("{key}.{k}");
                tag_span_from_value(span, &new_key, v, new_depth, max_depth);
            }
        }

        Value::Array(a) => {
            if a.is_empty() {
                span.meta.insert(key.to_string(), "[]".to_string());
                return;
            }

            for (i, v) in a.iter().enumerate() {
                let new_key = format!("{key}.{i}");
                tag_span_from_value(span, &new_key, v, new_depth, max_depth);
            }
        }
        Value::Null => {}
    }
}

fn redact_value(key: &str, value: String) -> String {
    let split_key = key.split('.').last().unwrap_or_default();
    if REDACTABLE_KEYS.contains(&split_key) {
        String::from("redacted")
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    #[test]
    fn test_simple_tagging() {
        let mut span = Span::default();
        let value = json!({ "request": { "simple": "value" } });

        tag_span_from_value(&mut span, "payload", &value, 0, 10);

        let expected = HashMap::from([("payload.request.simple".to_string(), "value".to_string())]);

        assert_eq!(span.meta, expected);
    }

    #[test]
    fn test_complex_object() {
        let mut span = Span::default();
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

        tag_span_from_value(&mut span, "payload", &value, 0, 10);

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

        assert_eq!(span.meta, expected);
    }

    #[test]
    fn test_array_of_objects() {
        let mut span = Span::default();
        let value = json!({
            "request": [
                { "simple": "value" },
                { "simple": "value" },
                { "simple": "value" },
            ]
        });

        tag_span_from_value(&mut span, "payload", &value, 0, 10);

        let expected = HashMap::from([
            ("payload.request.0.simple".to_string(), "value".to_string()),
            ("payload.request.1.simple".to_string(), "value".to_string()),
            ("payload.request.2.simple".to_string(), "value".to_string()),
        ]);

        assert_eq!(span.meta, expected);
    }

    #[test]
    fn test_reach_max_depth() {
        let mut span = Span::default();
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

        tag_span_from_value(&mut span, "payload", &value, 0, 2);

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

        assert_eq!(span.meta, expected);
    }

    #[test]
    fn test_tag_redacts_key() {
        let mut span = Span::default();
        let value = json!({
            "request": {
                "headers": {
                    "authorization": "secret token",
                }
            }
        });

        tag_span_from_value(&mut span, "payload", &value, 0, 10);

        let expected = HashMap::from([(
            "payload.request.headers.authorization".to_string(),
            "redacted".to_string(),
        )]);

        assert_eq!(span.meta, expected);
    }
}
