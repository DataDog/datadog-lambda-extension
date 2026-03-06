use datadog_opentelemetry::propagation::carrier::{Extractor, Injector};
use serde_json::Value;

/// Newtype wrapper around `serde_json::Value` to implement carrier traits
/// without violating the orphan rule.
pub struct JsonCarrier<'a>(pub &'a Value);

impl Extractor for JsonCarrier<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        if let Value::Object(map) = self.0 {
            map.get(&key.to_lowercase()).and_then(|v| v.as_str())
        } else {
            None
        }
    }

    fn keys(&self) -> Vec<&str> {
        if let Value::Object(map) = self.0 {
            map.keys().map(String::as_str).collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    }
}

pub struct JsonCarrierMut<'a>(pub &'a mut Value);

impl Injector for JsonCarrierMut<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let Value::Object(map) = self.0 {
            map.insert(key.to_lowercase(), Value::String(value));
        }
    }
}

#[cfg(test)]
mod test {
    use datadog_opentelemetry::propagation::carrier::{Extractor, Injector};

    use super::*;

    #[test]
    fn serde_value_get() {
        let mut value = Value::Object(serde_json::Map::new());
        JsonCarrierMut(&mut value).set("headerName", "value".to_string());

        let carrier = JsonCarrier(&value);
        assert_eq!(
            Extractor::get(&carrier, "HEADERNAME"),
            Some("value"),
            "case insensitive extraction"
        );
    }

    #[test]
    fn serde_value_keys() {
        let mut value = Value::Object(serde_json::Map::new());
        JsonCarrierMut(&mut value).set("headerName1", "value1".to_string());
        JsonCarrierMut(&mut value).set("headerName2", "value2".to_string());

        let carrier = JsonCarrier(&value);
        let got = Extractor::keys(&carrier);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&"headername1"));
        assert!(got.contains(&"headername2"));
    }
}
