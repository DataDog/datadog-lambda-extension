//! STS `GetCallerIdentity` signing for AWS delegated authentication
//!
//! Generates a signed STS `GetCallerIdentity` request that proves access to AWS credentials.
//! The proof is sent to Datadog's intake-key API to obtain a managed API key.

use base64::prelude::*;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Error;
use tracing::debug;

use crate::config::aws::AwsCredentials;

const GET_CALLER_IDENTITY_BODY: &str = "Action=GetCallerIdentity&Version=2011-06-15";
const CONTENT_TYPE: &str = "application/x-www-form-urlencoded; charset=utf-8";
const ORG_ID_HEADER: &str = "x-ddog-org-id";
const STS_SERVICE: &str = "sts";

/// Generates an authentication proof from AWS credentials.
///
/// The proof consists of a signed STS `GetCallerIdentity` request that can be
/// verified by Datadog's backend. The format is:
/// `base64(body)|base64(headers_json)|POST|base64(url)`
///
/// # Arguments
/// * `aws_credentials` - The AWS credentials to use for signing
/// * `region` - The AWS region for the STS endpoint
/// * `org_uuid` - The Datadog organization UUID to include in the signed headers
///
/// # Returns
/// The base64-encoded proof string, or an error if signing fails
#[allow(clippy::similar_names)]
pub fn generate_auth_proof(
    aws_credentials: &AwsCredentials,
    region: &str,
    org_uuid: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Generating delegated auth proof for region: {}", region);

    if aws_credentials.aws_access_key_id.is_empty()
        || aws_credentials.aws_secret_access_key.is_empty()
    {
        return Err("Missing AWS credentials for delegated auth".into());
    }

    if org_uuid.is_empty() {
        return Err("Missing org UUID for delegated auth".into());
    }

    let sts_host = if region.is_empty() {
        "sts.amazonaws.com".to_string()
    } else {
        format!("sts.{region}.amazonaws.com")
    };
    let sts_url = format!("https://{sts_host}");

    let now = Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    let payload_hash = hex::encode(Sha256::digest(GET_CALLER_IDENTITY_BODY.as_bytes()));

    // Canonical headers must be sorted alphabetically per SigV4 spec
    let canonical_headers = format!(
        "content-type:{CONTENT_TYPE}\nhost:{sts_host}\nx-amz-date:{amz_date}\n{ORG_ID_HEADER}:{org_uuid}"
    );

    let (signed_headers, canonical_headers) = if aws_credentials.aws_session_token.is_empty() {
        (
            "content-type;host;x-amz-date;x-ddog-org-id",
            canonical_headers,
        )
    } else {
        let headers = format!(
            "content-type:{CONTENT_TYPE}\nhost:{sts_host}\nx-amz-date:{amz_date}\nx-amz-security-token:{}\n{ORG_ID_HEADER}:{org_uuid}",
            aws_credentials.aws_session_token
        );
        (
            "content-type;host;x-amz-date;x-amz-security-token;x-ddog-org-id",
            headers,
        )
    };

    let canonical_request =
        format!("POST\n/\n\n{canonical_headers}\n\n{signed_headers}\n{payload_hash}");

    debug!(
        "Canonical request hash: {}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let algorithm = "AWS4-HMAC-SHA256";
    let effective_region = if region.is_empty() {
        "us-east-1"
    } else {
        region
    };
    let credential_scope = format!("{date_stamp}/{effective_region}/{STS_SERVICE}/aws4_request");
    let string_to_sign = format!(
        "{algorithm}\n{amz_date}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let signing_key = get_aws4_signature_key(
        &aws_credentials.aws_secret_access_key,
        &date_stamp,
        effective_region,
        STS_SERVICE,
    )?;

    let signature = hex::encode(sign(&signing_key, &string_to_sign)?);

    let authorization = format!(
        "{algorithm} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        aws_credentials.aws_access_key_id
    );

    // BTreeMap ensures consistent ordering for signature verification
    let mut headers_map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    headers_map.insert("Authorization".to_string(), vec![authorization]);
    headers_map.insert("Content-Type".to_string(), vec![CONTENT_TYPE.to_string()]);
    headers_map.insert("Host".to_string(), vec![sts_host]);
    headers_map.insert("X-Amz-Date".to_string(), vec![amz_date]);
    headers_map.insert("X-Ddog-Org-Id".to_string(), vec![org_uuid.to_string()]);

    if !aws_credentials.aws_session_token.is_empty() {
        headers_map.insert(
            "X-Amz-Security-Token".to_string(),
            vec![aws_credentials.aws_session_token.clone()],
        );
    }

    let headers_json = serde_json::to_string(&headers_map)?;

    // Proof format: base64(body)|base64(headers)|POST|base64(url)
    let proof = format!(
        "{}|{}|POST|{}",
        BASE64_STANDARD.encode(GET_CALLER_IDENTITY_BODY),
        BASE64_STANDARD.encode(&headers_json),
        BASE64_STANDARD.encode(&sts_url)
    );

    debug!("Generated delegated auth proof successfully");
    Ok(proof)
}

/// Signs a message using HMAC-SHA256
fn sign(key: &[u8], msg: &str) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|err| {
        Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Error creating HMAC: {err}"),
        )
    })?;
    mac.update(msg.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

/// Derives the AWS `SigV4` signing key
fn get_aws4_signature_key(
    key: &str,
    date_stamp: &str,
    region_name: &str,
    service_name: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let k_date = sign(format!("AWS4{key}").as_bytes(), date_stamp)?;
    let k_region = sign(&k_date, region_name)?;
    let k_service = sign(&k_region, service_name)?;
    sign(&k_service, "aws4_request")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_auth_proof_missing_credentials() {
        let creds = AwsCredentials {
            aws_access_key_id: String::new(),
            aws_secret_access_key: String::new(),
            aws_session_token: String::new(),
            aws_container_credentials_full_uri: String::new(),
            aws_container_authorization_token: String::new(),
        };

        let result = generate_auth_proof(&creds, "us-east-1", "test-org-uuid");
        assert!(result.is_err());
        assert!(
            result
                .expect_err("expected error for missing credentials")
                .to_string()
                .contains("Missing AWS credentials")
        );
    }

    #[test]
    fn test_generate_auth_proof_missing_org_uuid() {
        let creds = AwsCredentials {
            aws_access_key_id: "AKIDEXAMPLE".to_string(),
            aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            aws_session_token: String::new(),
            aws_container_credentials_full_uri: String::new(),
            aws_container_authorization_token: String::new(),
        };

        let result = generate_auth_proof(&creds, "us-east-1", "");
        assert!(result.is_err());
        assert!(
            result
                .expect_err("expected error for missing org UUID")
                .to_string()
                .contains("Missing org UUID")
        );
    }

    #[test]
    fn test_generate_auth_proof_format() {
        let creds = AwsCredentials {
            aws_access_key_id: "AKIDEXAMPLE".to_string(),
            aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            aws_session_token: "AQoDYXdzEJrtoken".to_string(),
            aws_container_credentials_full_uri: String::new(),
            aws_container_authorization_token: String::new(),
        };

        let result = generate_auth_proof(&creds, "us-east-1", "test-org-uuid");
        assert!(result.is_ok());

        let proof = result.expect("failed to generate auth proof");
        let parts: Vec<&str> = proof.split('|').collect();
        assert_eq!(parts.len(), 4);

        // Verify body is base64-encoded GET_CALLER_IDENTITY_BODY
        let body = String::from_utf8(
            BASE64_STANDARD
                .decode(parts[0])
                .expect("Failed to decode base64 body"),
        )
        .expect("Failed to convert body to UTF-8");
        assert_eq!(body, GET_CALLER_IDENTITY_BODY);

        // Verify method
        assert_eq!(parts[2], "POST");

        // Verify URL is base64-encoded STS URL
        let url = String::from_utf8(
            BASE64_STANDARD
                .decode(parts[3])
                .expect("Failed to decode base64 URL"),
        )
        .expect("Failed to convert URL to UTF-8");
        assert!(url.contains("sts.us-east-1.amazonaws.com"));
    }

    #[test]
    fn test_generate_auth_proof_headers_contain_required_fields() {
        let creds = AwsCredentials {
            aws_access_key_id: "AKIDEXAMPLE".to_string(),
            aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            aws_session_token: "AQoDYXdzEJrtoken".to_string(),
            aws_container_credentials_full_uri: String::new(),
            aws_container_authorization_token: String::new(),
        };

        let result = generate_auth_proof(&creds, "us-east-1", "my-org-uuid");
        assert!(result.is_ok());

        let proof = result.expect("Failed to generate auth proof");
        let parts: Vec<&str> = proof.split('|').collect();

        // Decode and parse headers
        let headers_json = String::from_utf8(
            BASE64_STANDARD
                .decode(parts[1])
                .expect("Failed to decode base64 headers"),
        )
        .expect("Failed to convert headers to UTF-8");
        let headers: BTreeMap<String, Vec<String>> =
            serde_json::from_str(&headers_json).expect("Failed to parse headers JSON");

        // Verify required headers (canonical casing)
        assert!(headers.contains_key("Authorization"));
        assert!(headers.contains_key("Content-Type"));
        assert!(headers.contains_key("Host"));
        assert!(headers.contains_key("X-Amz-Date"));
        assert!(headers.contains_key("X-Amz-Security-Token"));
        assert!(headers.contains_key("X-Ddog-Org-Id"));

        // Verify org-id header value (array format)
        assert_eq!(
            headers
                .get("X-Ddog-Org-Id")
                .expect("Missing X-Ddog-Org-Id header"),
            &vec!["my-org-uuid".to_string()]
        );
    }

    #[test]
    fn test_generate_auth_proof_default_region() {
        let creds = AwsCredentials {
            aws_access_key_id: "AKIDEXAMPLE".to_string(),
            aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            aws_session_token: String::new(),
            aws_container_credentials_full_uri: String::new(),
            aws_container_authorization_token: String::new(),
        };

        // Empty region should use global STS endpoint
        let result = generate_auth_proof(&creds, "", "test-org-uuid");
        assert!(result.is_ok());

        let proof = result.expect("Failed to generate auth proof");
        let parts: Vec<&str> = proof.split('|').collect();
        let url = String::from_utf8(
            BASE64_STANDARD
                .decode(parts[3])
                .expect("Failed to decode base64 URL"),
        )
        .expect("Failed to convert URL to UTF-8");
        assert!(url.contains("sts.amazonaws.com"));
    }
}
