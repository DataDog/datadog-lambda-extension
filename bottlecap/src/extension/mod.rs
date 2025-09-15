use reqwest::Client;
use serde::Deserialize;
use std::env;
use tracing::error;

pub const EXTENSION_HOST: &str = "0.0.0.0";
pub const EXTENSION_HOST_IP: [u8; 4] = [0, 0, 0, 0];
pub const EXTENSION_NAME: &str = "datadog-agent";
pub const EXTENSION_FEATURES: &str = "accountId";
pub const EXTENSION_NAME_HEADER: &str = "Lambda-Extension-Name";
pub const EXTENSION_ID_HEADER: &str = "Lambda-Extension-Identifier";
pub const EXTENSION_ACCEPT_FEATURE_HEADER: &str = "Lambda-Extension-Accept-Feature";
pub const EXTENSION_ROUTE: &str = "2020-01-01/extension";

/// Error conditions that can arise from extension operations
#[derive(thiserror::Error, Debug)]
pub enum ExtensionError {
    /// Environment variable error
    #[error("Environment variable error: {0}")]
    EnvVarError(#[from] env::VarError),

    /// HTTP request error
    #[error("HTTP request error: {0}")]
    HttpError(#[from] reqwest::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Extension registration failed
    #[error("Failed to register extension: {0}")]
    RegistrationFailed(String),

    /// Extension ID header not found in response
    #[error("Extension ID header not found in registration response")]
    MissingExtensionIdHeader,

    /// Extension ID header cannot be converted to string
    #[error("Extension ID header cannot be converted to string")]
    InvalidExtensionIdHeader,

    /// Failed to read response body
    #[error("Failed to read response body")]
    ResponseBodyReadError,

    /// HTTP error with status code
    #[error("HTTP error with status: {status}")]
    HttpStatusError { status: u16 },
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Response from the register endpoint
/// <https://docs.aws.amazon.com/lambda/latest/dg/runtimes-extensions-api.html#runtimes-extensions-registration-api>
pub struct RegisterResponse {
    // Skip deserialize because this field is not available in the response
    // body, but as a header. Header is extracted and set manually.
    #[serde(skip_deserializing)]
    pub extension_id: String,
    pub account_id: Option<String>,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(tag = "eventType")]
pub enum NextEventResponse {
    #[serde(rename(deserialize = "INVOKE"), rename_all = "camelCase")]
    Invoke {
        deadline_ms: u64,
        request_id: String,
        invoked_function_arn: String,
    },
    #[serde(rename(deserialize = "SHUTDOWN"), rename_all = "camelCase")]
    Shutdown {
        shutdown_reason: String,
        deadline_ms: u64,
    },
}

/// Return the base URL for the Lambda Runtime API
///
#[must_use]
pub fn base_url(route: &str, runtime_api: &str) -> String {
    format!("http://{runtime_api}/{route}")
}

pub async fn register(
    client: &Client,
    runtime_api: &str,
) -> Result<RegisterResponse, ExtensionError> {
    let events_to_subscribe_to = serde_json::json!({
        "events": ["INVOKE", "SHUTDOWN"]
    });

    let base_url = base_url(EXTENSION_ROUTE, runtime_api);
    let url = format!("{base_url}/register");

    let response = client
        .post(&url)
        .header(EXTENSION_NAME_HEADER, EXTENSION_NAME)
        .header(EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES)
        .json(&events_to_subscribe_to)
        .send()
        .await?;

    if response.status() != 200 {
        let status = response.status().as_u16();
        return Err(ExtensionError::HttpStatusError { status });
    }

    let extension_id = response
        .headers()
        .get(EXTENSION_ID_HEADER)
        .ok_or(ExtensionError::MissingExtensionIdHeader)?
        .to_str()
        .map_err(|_| ExtensionError::InvalidExtensionIdHeader)?
        .to_string();

    let mut register_response = response.json::<RegisterResponse>().await?;

    // Set manually since it's not part of the response body
    register_response.extension_id = extension_id;

    Ok(register_response)
}

pub async fn next_event(
    client: &Client,
    runtime_api: &str,
    extension_id: &str,
) -> Result<NextEventResponse, ExtensionError> {
    let base_url = base_url(EXTENSION_ROUTE, runtime_api);
    let url = format!("{base_url}/event/next");

    let response = client
        .get(&url)
        .header(EXTENSION_ID_HEADER, extension_id)
        .send()
        .await?;

    let status = response.status();

    let text = match response.text().await {
        Ok(t) => t,
        Err(e) => {
            error!("Next response: Failed to read response body: {}", e);
            return Err(ExtensionError::ResponseBodyReadError);
        }
    };

    if !status.is_success() {
        error!("Next response HTTP Error {} - Response: {}", status, text);
        return Err(ExtensionError::HttpStatusError {
            status: status.as_u16(),
        });
    }

    let next_event_response = serde_json::from_str(&text)?;
    Ok(next_event_response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_event_response() {
        let response = r#"{
            "eventType": "INVOKE",
            "deadlineMs": 676051,
            "requestId": "3da1f2dc-3222-475e-9205-e2e6c6318895",
            "invokedFunctionArn": "arn:aws:lambda:us-east-1:123456789012:function:ExtensionTest",
            "tracing": {
                "type": "X-Amzn-Trace-Id",
                "value": "Root=1-5f35ae12-0c0fec141ab77a00bc047aa2;Parent=2be948a625588e32;Sampled=1"
            }
        }"#;
        let response: NextEventResponse =
            serde_json::from_str(response).expect("Deserialization failed");
        assert_eq!(
            response,
            NextEventResponse::Invoke {
                deadline_ms: 676_051,
                request_id: "3da1f2dc-3222-475e-9205-e2e6c6318895".to_string(),
                invoked_function_arn:
                    "arn:aws:lambda:us-east-1:123456789012:function:ExtensionTest".to_string()
            }
        );
    }

    #[test]
    fn test_next_event_response_shutdown() {
        let response = r#"{
            "eventType": "SHUTDOWN",
            "shutdownReason": "SPINDOWN",
            "deadlineMs": 676051
        }"#;
        let response: NextEventResponse =
            serde_json::from_str(response).expect("Deserialization failed");
        assert_eq!(
            response,
            NextEventResponse::Shutdown {
                shutdown_reason: "SPINDOWN".to_string(),
                deadline_ms: 676_051,
            }
        );
    }
}
