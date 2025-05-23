/// Code inspired, and copied, by OpenTelemetry Rust project.
/// <https://github.com/open-telemetry/opentelemetry-rust/blob/main/opentelemetry/src/propagation/mod.rs>
///
use std::collections::HashMap;

use serde_json::Value;

/// Injector provides an interface for a carrier to be used
/// with a Propagator to inject a Context into the carrier.
///
pub trait Injector {
    /// Set a value in the carrier.
    fn set(&mut self, key: &str, value: String);
}

pub trait Extractor {
    /// Get a value from the carrier.
    fn get(&self, key: &str) -> Option<&str>;

    /// Get all keys from the carrier.
    fn keys(&self) -> Vec<&str>;
}

impl<S: std::hash::BuildHasher> Injector for HashMap<String, String, S> {
    /// Set a key and value in the `HashMap`.
    fn set(&mut self, key: &str, value: String) {
        self.insert(key.to_lowercase(), value);
    }
}

impl<S: std::hash::BuildHasher> Extractor for HashMap<String, String, S> {
    /// Get a value for a key from the `HashMap`.
    fn get(&self, key: &str) -> Option<&str> {
        self.get(&key.to_lowercase()).map(String::as_str)
    }

    /// Collect all the keys from the `HashMap`.
    fn keys(&self) -> Vec<&str> {
        self.keys().map(String::as_str).collect::<Vec<_>>()
    }
}

impl Injector for Value {
    /// Set a key and value in the `Value`.
    fn set(&mut self, key: &str, value: String) {
        if let Value::Object(map) = self {
            map.insert(key.to_lowercase(), Value::String(value));
        }
    }
}

impl Extractor for Value {
    /// Get a value for a key from the `Value`.
    fn get(&self, key: &str) -> Option<&str> {
        if let Value::Object(map) = self {
            map.get(&key.to_lowercase()).and_then(|v| v.as_str())
        } else {
            None
        }
    }

    /// Collect all the keys from the `Value`.
    fn keys(&self) -> Vec<&str> {
        if let Value::Object(map) = self {
            map.keys().map(String::as_str).collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn hash_map_get() {
        let mut carrier = HashMap::new();
        carrier.set("headerName", "value".to_string());

        assert_eq!(
            Extractor::get(&carrier, "HEADERNAME"),
            Some("value"),
            "case insensitive extraction"
        );
    }

    #[test]
    fn hash_map_keys() {
        let mut carrier = HashMap::new();
        carrier.set("headerName1", "value1".to_string());
        carrier.set("headerName2", "value2".to_string());

        let got = Extractor::keys(&carrier);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&"headername1"));
        assert!(got.contains(&"headername2"));
    }

    #[test]
    fn serde_value_get() {
        let mut carrier = Value::Object(serde_json::Map::new());
        carrier.set("headerName", "value".to_string());

        assert_eq!(
            Extractor::get(&carrier, "HEADERNAME"),
            Some("value"),
            "case insensitive extraction"
        );
    }

    #[test]
    fn serde_value_keys() {
        let mut carrier = Value::Object(serde_json::Map::new());
        carrier.set("headerName1", "value1".to_string());
        carrier.set("headerName2", "value2".to_string());

        let got = Extractor::keys(&carrier);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&"headername1"));
        assert!(got.contains(&"headername2"));
    }
}
