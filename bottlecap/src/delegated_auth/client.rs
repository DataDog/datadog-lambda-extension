//! Datadog intake-key API client for delegated authentication
//!
//! Exchanges the signed STS `GetCallerIdentity` proof for a managed API key.

use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, error, info};

use crate::config::Config;
use crate::config::aws::{AwsConfig, AwsCredentials};
use crate::delegated_auth::auth_proof::generate_auth_proof;

const INTAKE_KEY_ENDPOINT: &str = "/api/v2/intake-key";

#[derive(Debug, Deserialize)]
struct IntakeKeyResponse {
    data: IntakeKeyData,
}

#[derive(Debug, Deserialize)]
struct IntakeKeyData {
    attributes: IntakeKeyAttributes,
}

#[derive(Debug, Deserialize)]
struct IntakeKeyAttributes {
    api_key: String,
}

/// Gets a delegated API key from Datadog using AWS credentials.
///
/// This function:
/// 1. Generates a signed STS `GetCallerIdentity` proof
/// 2. Sends the proof to Datadog's intake-key API
/// 3. Returns the managed API key
///
/// # Arguments
/// * `config` - The extension configuration containing site and `org_uuid`
/// * `aws_config` - The AWS configuration containing region
///
/// # Returns
/// The API key string, or an error if the request fails
pub async fn get_delegated_api_key(
    config: &Arc<Config>,
    aws_config: &Arc<AwsConfig>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Attempting to get API key via delegated auth");

    let mut aws_credentials = AwsCredentials::from_env();

    // SnapStart: credentials come from the container endpoint instead of env vars
    if aws_credentials.aws_secret_access_key.is_empty()
        && aws_credentials.aws_access_key_id.is_empty()
        && !aws_credentials
            .aws_container_credentials_full_uri
            .is_empty()
        && !aws_credentials.aws_container_authorization_token.is_empty()
    {
        debug!("Fetching credentials from container credentials endpoint (SnapStart)");
        aws_credentials = get_snapstart_credentials(&aws_credentials).await?;
    }

    let proof = generate_auth_proof(&aws_credentials, &aws_config.region, &config.org_uuid)?;

    let url = get_api_endpoint(&config.site);
    info!("Requesting delegated API key from: {}", url);

    let builder = match create_reqwest_client_builder() {
        Ok(b) => b,
        Err(err) => {
            error!("Error creating reqwest client builder: {}", err);
            return Err(err.to_string().into());
        }
    };

    let client = match builder.build() {
        Ok(c) => c,
        Err(err) => {
            error!("Error creating reqwest client: {}", err);
            return Err(err.into());
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Delegated {proof}"))?,
    );

    let response = client
        .post(&url)
        .headers(headers)
        .body("")
        .send()
        .await
        .map_err(|err| {
            error!("Error sending delegated auth request: {}", err);
            err
        })?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let err_msg = format!(
            "Delegated auth request failed with status {status} (response body length: {} bytes)",
            body.len()
        );
        error!("{err_msg}");
        return Err(err_msg.into());
    }

    let response_body = response.text().await?;
    let parsed: IntakeKeyResponse = serde_json::from_str(&response_body).map_err(|err| {
        error!(
            "Failed to parse delegated auth response: {} - body: {}",
            err, response_body
        );
        err
    })?;

    let api_key = parsed.data.attributes.api_key;
    if api_key.is_empty() {
        return Err("Received empty API key from delegated auth".into());
    }

    info!("Delegated auth API key obtained successfully");
    Ok(api_key)
}

/// Gets the API endpoint URL based on the site configuration.
///
/// Maps the `DD_SITE` value to the appropriate API endpoint:
/// - datadoghq.com -> api.datadoghq.com
/// - us3.datadoghq.com -> api.us3.datadoghq.com
/// - datadoghq.eu -> api.datadoghq.eu
/// - ddog-gov.com -> api.ddog-gov.com
fn get_api_endpoint(site: &str) -> String {
    let site = site.trim();

    if site.is_empty() {
        return format!("https://api.datadoghq.com{INTAKE_KEY_ENDPOINT}");
    }

    let domain = if site.starts_with("https://") || site.starts_with("http://") {
        site.split("://")
            .nth(1)
            .unwrap_or(site)
            .split('/')
            .next()
            .unwrap_or(site)
    } else {
        site
    };

    format!("https://api.{domain}{INTAKE_KEY_ENDPOINT}")
}

/// Fetches credentials from the container credentials endpoint (for `SnapStart`).
async fn get_snapstart_credentials(
    aws_credentials: &AwsCredentials,
) -> Result<AwsCredentials, Box<dyn std::error::Error + Send + Sync>> {
    let builder = create_reqwest_client_builder().map_err(|e| e.to_string())?;
    let client = builder.build()?;

    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&aws_credentials.aws_container_authorization_token)?,
    );

    let response = client
        .get(&aws_credentials.aws_container_credentials_full_uri)
        .headers(headers)
        .send()
        .await?;

    let body = response.text().await?;
    let creds: serde_json::Value = serde_json::from_str(&body)?;

    Ok(AwsCredentials {
        aws_access_key_id: creds["AccessKeyId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        aws_secret_access_key: creds["SecretAccessKey"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        aws_session_token: creds["Token"].as_str().unwrap_or_default().to_string(),
        aws_container_credentials_full_uri: String::new(),
        aws_container_authorization_token: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_api_endpoint_default() {
        assert_eq!(
            get_api_endpoint("datadoghq.com"),
            "https://api.datadoghq.com/api/v2/intake-key"
        );
    }

    #[test]
    fn test_get_api_endpoint_us3() {
        assert_eq!(
            get_api_endpoint("us3.datadoghq.com"),
            "https://api.us3.datadoghq.com/api/v2/intake-key"
        );
    }

    #[test]
    fn test_get_api_endpoint_us5() {
        assert_eq!(
            get_api_endpoint("us5.datadoghq.com"),
            "https://api.us5.datadoghq.com/api/v2/intake-key"
        );
    }

    #[test]
    fn test_get_api_endpoint_eu() {
        assert_eq!(
            get_api_endpoint("datadoghq.eu"),
            "https://api.datadoghq.eu/api/v2/intake-key"
        );
    }

    #[test]
    fn test_get_api_endpoint_gov() {
        assert_eq!(
            get_api_endpoint("ddog-gov.com"),
            "https://api.ddog-gov.com/api/v2/intake-key"
        );
    }

    #[test]
    fn test_get_api_endpoint_ap1() {
        assert_eq!(
            get_api_endpoint("ap1.datadoghq.com"),
            "https://api.ap1.datadoghq.com/api/v2/intake-key"
        );
    }

    #[test]
    fn test_get_api_endpoint_empty() {
        assert_eq!(
            get_api_endpoint(""),
            "https://api.datadoghq.com/api/v2/intake-key"
        );
    }

    #[test]
    fn test_get_api_endpoint_with_protocol() {
        assert_eq!(
            get_api_endpoint("https://datadoghq.com"),
            "https://api.datadoghq.com/api/v2/intake-key"
        );
    }
}
