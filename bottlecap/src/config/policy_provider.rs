//! Policy provider configuration types and deserialization.
//!
//! This module provides configuration types for policy providers that can be
//! loaded from environment variables or YAML files.

use policy_rs::config::{
    FileProviderConfig, Header as PolicyRsHeader, HttpProviderConfig,
    ProviderConfig as PolicyRsProviderConfig,
};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use tracing::error;

/// Configuration for a policy provider.
///
/// Supports file and HTTP providers with their respective configuration options.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PolicyProviderConfig {
    /// File-based policy provider
    File {
        /// Unique identifier for this provider
        id: String,
        /// Path to the policy JSON file
        path: String,
    },
    /// HTTP-based policy provider with polling
    Http {
        /// Unique identifier for this provider
        id: String,
        /// URL to fetch policies from
        url: String,
        /// Optional HTTP headers (e.g., for authentication)
        #[serde(default)]
        headers: Vec<HttpHeader>,
        /// Polling interval in seconds
        #[serde(default = "default_poll_interval")]
        poll_interval_secs: u64,
    },
}

impl PolicyProviderConfig {
    /// Converts to policy-rs's `ProviderConfig` type.
    #[must_use]
    pub fn to_policy_rs_config(&self) -> PolicyRsProviderConfig {
        match self {
            PolicyProviderConfig::File { id, path } => {
                PolicyRsProviderConfig::File(FileProviderConfig {
                    id: id.clone(),
                    path: path.clone(),
                })
            }
            PolicyProviderConfig::Http {
                id,
                url,
                headers,
                poll_interval_secs,
            } => PolicyRsProviderConfig::Http(HttpProviderConfig {
                id: id.clone(),
                url: url.clone(),
                headers: headers
                    .iter()
                    .map(|h| PolicyRsHeader {
                        name: h.name.clone(),
                        value: h.value.clone(),
                    })
                    .collect(),
                poll_interval_secs: Some(*poll_interval_secs),
                content_type: None,
            }),
        }
    }
}

/// HTTP header configuration for HTTP provider
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

fn default_poll_interval() -> u64 {
    60
}

/// Deserialize policy providers from JSON string or array.
///
/// Supports both a JSON string (from environment variables) and a direct array
/// (from YAML configuration).
pub fn deserialize_policy_providers<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<PolicyProviderConfig>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: JsonValue = Deserialize::deserialize(deserializer)?;

    match value {
        JsonValue::Null => Ok(None),
        JsonValue::String(s) if s.trim().is_empty() => Ok(None),
        JsonValue::String(s) => match serde_json::from_str(&s) {
            Ok(providers) => Ok(Some(providers)),
            Err(e) => {
                error!(
                    "Failed to parse policy providers from string: {}, ignoring",
                    e
                );
                Ok(None)
            }
        },
        JsonValue::Array(arr) => {
            let mut providers = Vec::new();
            for item in arr {
                match serde_json::from_value(item.clone()) {
                    Ok(provider) => providers.push(provider),
                    Err(e) => {
                        error!("Failed to parse policy provider: {}, ignoring", e);
                    }
                }
            }
            if providers.is_empty() {
                Ok(None)
            } else {
                Ok(Some(providers))
            }
        }
        _ => {
            error!("Invalid policy providers value, expected string or array, ignoring");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_file_provider() {
        let json = r#"{"id": "local", "type": "file", "path": "policies.json"}"#;
        let provider: PolicyProviderConfig = serde_json::from_str(json).unwrap();

        assert_eq!(
            provider,
            PolicyProviderConfig::File {
                id: "local".to_string(),
                path: "policies.json".to_string(),
            }
        );
    }

    #[test]
    fn test_deserialize_http_provider() {
        let json = r#"{
            "id": "remote",
            "type": "http",
            "url": "https://api.example.com/policies",
            "headers": [{"name": "Authorization", "value": "Bearer token"}],
            "poll_interval_secs": 30
        }"#;
        let provider: PolicyProviderConfig = serde_json::from_str(json).unwrap();

        assert_eq!(
            provider,
            PolicyProviderConfig::Http {
                id: "remote".to_string(),
                url: "https://api.example.com/policies".to_string(),
                headers: vec![HttpHeader {
                    name: "Authorization".to_string(),
                    value: "Bearer token".to_string(),
                }],
                poll_interval_secs: 30,
            }
        );
    }

    #[test]
    fn test_deserialize_http_provider_defaults() {
        let json = r#"{
            "id": "remote",
            "type": "http",
            "url": "https://api.example.com/policies"
        }"#;
        let provider: PolicyProviderConfig = serde_json::from_str(json).unwrap();

        assert_eq!(
            provider,
            PolicyProviderConfig::Http {
                id: "remote".to_string(),
                url: "https://api.example.com/policies".to_string(),
                headers: vec![],
                poll_interval_secs: 60,
            }
        );
    }

    #[test]
    fn test_deserialize_providers_array() {
        let json = r#"[
            {"id": "local", "type": "file", "path": "policies.json"},
            {"id": "remote", "type": "http", "url": "https://api.example.com/policies"}
        ]"#;
        let providers: Vec<PolicyProviderConfig> = serde_json::from_str(json).unwrap();

        assert_eq!(providers.len(), 2);
        assert!(matches!(providers[0], PolicyProviderConfig::File { .. }));
        assert!(matches!(providers[1], PolicyProviderConfig::Http { .. }));
    }
}
