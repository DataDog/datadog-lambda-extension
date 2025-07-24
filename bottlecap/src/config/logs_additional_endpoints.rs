use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tracing::error;

#[derive(Debug, PartialEq, Clone, Deserialize)]
pub struct LogsAdditionalEndpoint {
    pub api_key: String,
    #[serde(rename = "Host")]
    pub host: String,
    #[serde(rename = "Port")]
    pub port: u32,
    pub is_reliable: bool,
}

#[allow(clippy::module_name_repetitions)]
pub fn deserialize_logs_additional_endpoints<'de, D>(
    deserializer: D,
) -> Result<Vec<LogsAdditionalEndpoint>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;

    match value {
        Value::String(s) if !s.is_empty() => {
            // For JSON format (string) in DD_ADDITIONAL_ENDPOINTS
            Ok(serde_json::from_str(&s).unwrap_or_else(|err| {
                error!("Failed to deserialize DD_LOGS_CONFIG_ADDITIONAL_ENDPOINTS: {err}");
                vec![]
            }))
        }
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_deserialize_logs_additional_endpoints_valid() {
        let input = json!(
            "[{\"api_key\": \"apiKey2\", \"Host\": \"agent-http-intake.logs.datadoghq.com\", \"Port\": 443, \"is_reliable\": true}]"
        );

        let result = deserialize_logs_additional_endpoints(input)
            .expect("Failed to deserialize logs additional endpoints");
        let expected = vec![LogsAdditionalEndpoint {
            api_key: "apiKey2".to_string(),
            host: "agent-http-intake.logs.datadoghq.com".to_string(),
            port: 443,
            is_reliable: true,
        }];

        assert_eq!(result, expected);
    }

    #[test]
    fn test_deserialize_logs_additional_endpoints_invalid() {
        // input missing "Port" field
        let input = json!(
            "[{\"api_key\": \"apiKey2\", \"Host\": \"agent-http-intake.logs.datadoghq.com\", \"is_reliable\": true}]"
        );

        let result = deserialize_logs_additional_endpoints(input)
            .expect("Failed to deserialize logs additional endpoints");
        let expected = Vec::new(); // expect empty list due to invalid input

        assert_eq!(result, expected);
    }
}
