use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use std::sync::Arc;
use tracing::{debug, error};

use super::auth_proof::generate_auth_proof;
use crate::config::Config;
use crate::config::aws::{AwsConfig, AwsCredentials};

const INTAKE_KEY_ENDPOINT: &str = "/api/v2/intake-key";

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
/// * `client` - A pre-built `reqwest::Client` to use for the request. The client is built
///   with `create_reqwest_client_builder()` which respects proxy configuration via
///   `HTTPS_PROXY` environment variables, so hardcoded `https://` in the URL does not
///   bypass proxy settings.
/// * `aws_credentials` - Pre-resolved AWS credentials (`SnapStart` credentials must be
///   fetched by the caller before invoking this function)
///
/// # Returns
/// The API key string, or an error if the request fails
pub async fn get_delegated_api_key(
    config: &Arc<Config>,
    aws_config: &Arc<AwsConfig>,
    client: &reqwest::Client,
    aws_credentials: &AwsCredentials,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Attempting to get API key via delegated auth");

    let proof = generate_auth_proof(aws_credentials, &aws_config.region, &config.dd_org_uuid)?;

    let url = get_api_endpoint(&config.site);
    debug!("Requesting delegated API key from: {}", url);

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
    let response_body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let err_msg = format!(
            "Delegated auth request failed with status {status} (response body length: {} bytes)",
            response_body.len()
        );
        error!("{err_msg}");
        return Err(err_msg.into());
    }

    let parsed: serde_json::Value = serde_json::from_str(&response_body).map_err(|err| {
        error!(
            "Failed to parse delegated auth response: {} (body length: {} bytes)",
            err,
            response_body.len()
        );
        err
    })?;

    let api_key = parsed["data"]["attributes"]["api_key"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    if api_key.is_empty() {
        return Err("Received empty API key from delegated auth".into());
    }

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
