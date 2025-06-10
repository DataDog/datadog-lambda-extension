use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::HashMap;
use tracing::error;

#[allow(clippy::module_name_repetitions)]
pub fn deserialize_additional_endpoints<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;

    match value {
        Value::Object(map) => {
            // For YAML format (object) in datadog.yaml
            let mut result = HashMap::new();
            for (key, value) in map {
                match value {
                    Value::Array(arr) => {
                        let urls: Vec<String> = arr
                            .into_iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();
                        result.insert(key, urls);
                    }
                    _ => {
                        error!("Failed to deserialize additional endpoints - Invalid YAML format: expected array for key {}", key);
                    }
                }
            }
            Ok(result)
        }
        Value::String(s) if !s.is_empty() => {
            // For JSON format (string) in DD_ADDITIONAL_ENDPOINTS
            if let Ok(map) = serde_json::from_str(&s) {
                Ok(map)
            } else {
                error!("Failed to deserialize additional endpoints - Invalid JSON format");
                Ok(HashMap::new())
            }
        }
        _ => Ok(HashMap::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deserialize_additional_endpoints_yaml() {
        // Test YAML format (object)
        let input = json!({
            "https://app.datadoghq.com": ["key1", "key2"],
            "https://app.datadoghq.eu": ["key3"]
        });

        let result = deserialize_additional_endpoints(input).unwrap();

        let mut expected = HashMap::new();
        expected.insert(
            "https://app.datadoghq.com".to_string(),
            vec!["key1".to_string(), "key2".to_string()],
        );
        expected.insert(
            "https://app.datadoghq.eu".to_string(),
            vec!["key3".to_string()],
        );

        assert_eq!(result, expected);
    }

    #[test]
    fn test_deserialize_additional_endpoints_json() {
        // Test JSON string format
        let input = json!("{\"https://app.datadoghq.com\":[\"key1\",\"key2\"],\"https://app.datadoghq.eu\":[\"key3\"]}");

        let result = deserialize_additional_endpoints(input).unwrap();

        let mut expected = HashMap::new();
        expected.insert(
            "https://app.datadoghq.com".to_string(),
            vec!["key1".to_string(), "key2".to_string()],
        );
        expected.insert(
            "https://app.datadoghq.eu".to_string(),
            vec!["key3".to_string()],
        );

        assert_eq!(result, expected);
    }

    #[test]
    fn test_deserialize_additional_endpoints_invalid_or_empty() {
        // Test empty YAML
        let input = json!({});
        let result = deserialize_additional_endpoints(input).unwrap();
        assert!(result.is_empty());

        // Test empty JSON
        let input = json!("");
        let result = deserialize_additional_endpoints(input).unwrap();
        assert!(result.is_empty());

        let input = json!({
            "https://app.datadoghq.com": "invalid-yaml"
        });
        let result = deserialize_additional_endpoints(input).unwrap();
        assert!(result.is_empty());

        let input = json!("invalid-json");
        let result = deserialize_additional_endpoints(input).unwrap();
        assert!(result.is_empty());
    }
}
