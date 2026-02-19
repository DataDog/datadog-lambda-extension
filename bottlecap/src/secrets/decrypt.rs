use crate::config::{
    Config,
    aws::{AwsConfig, AwsCredentials},
};
use crate::fips::compute_aws_api_host;
use base64::prelude::*;
use chrono::{DateTime, Utc};
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use hmac::{Hmac, Mac};
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Error;
use std::sync::Arc;
use tokio::time::Instant;
use tracing::debug;
use tracing::error;

pub async fn resolve_secrets(config: Arc<Config>, aws_config: Arc<AwsConfig>) -> Option<String> {
    let api_key_candidate = if !config.api_key_secret_arn.is_empty()
        || !config.kms_api_key.is_empty()
        || !config.api_key_ssm_arn.is_empty()
    {
        let before_decrypt = Instant::now();

        let builder = match create_reqwest_client_builder() {
            Ok(builder) => builder,
            Err(err) => {
                error!("Error creating reqwest client builder: {}", err);
                return None;
            }
        };

        let client = match builder.build() {
            Ok(client) => client,
            Err(err) => {
                error!("Error creating reqwest client: {}", err);
                return None;
            }
        };

        let mut aws_credentials = AwsCredentials::from_env();

        if aws_credentials.aws_secret_access_key.is_empty()
            && aws_credentials.aws_access_key_id.is_empty()
            && !aws_credentials
                .aws_container_credentials_full_uri
                .is_empty()
            && !aws_credentials.aws_container_authorization_token.is_empty()
        {
            // We're in Snap Start
            let credentials = match get_snapstart_credentials(&aws_credentials, &client).await {
                Ok(credentials) => credentials,
                Err(err) => {
                    error!("Error getting Snap Start credentials: {}", err);
                    return None;
                }
            };
            aws_credentials.aws_access_key_id = credentials["AccessKeyId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            aws_credentials.aws_secret_access_key = credentials["SecretAccessKey"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            aws_credentials.aws_session_token = credentials["Token"]
                .as_str()
                .unwrap_or_default()
                .to_string();
        }

        let decrypted_key = if !config.kms_api_key.is_empty() {
            decrypt_aws_kms(
                &client,
                config.kms_api_key.clone(),
                aws_config,
                &aws_credentials,
            )
            .await
        } else if !config.api_key_secret_arn.is_empty() {
            decrypt_aws_sm(
                &client,
                config.api_key_secret_arn.clone(),
                aws_config,
                &aws_credentials,
            )
            .await
        } else {
            decrypt_aws_ssm(
                &client,
                config.api_key_ssm_arn.clone(),
                aws_config,
                &aws_credentials,
            )
            .await
        };

        debug!("Decrypt took {} ms", before_decrypt.elapsed().as_millis());

        match decrypted_key {
            Ok(key) => Some(key),
            Err(err) => {
                error!("Error decrypting key: {}", err);
                None
            }
        }
    } else {
        Some(config.api_key.clone())
    };

    clean_api_key(api_key_candidate)
}

fn clean_api_key(maybe_key: Option<String>) -> Option<String> {
    if let Some(key) = maybe_key {
        let clean_key = key.trim_end_matches('\n').replace(' ', "").clone();
        if !clean_key.is_empty() {
            return Some(clean_key);
        }
        error!("API key has invalid format");
    }
    None
}

struct RequestArgs<'a> {
    service: String,
    body: &'a Value,
    time: DateTime<Utc>,
    x_amz_target: String,
}

async fn decrypt_aws_kms(
    client: &Client,
    kms_key: String,
    aws_config: Arc<AwsConfig>,
    aws_credentials: &AwsCredentials,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // When the API key is encrypted using the AWS console, the function name is added as an
    // encryption context. When the API key is encrypted using the AWS CLI, no encryption context
    // is added. We need to try decrypting the API key both with and without the encryption context.

    let json_body = &serde_json::json!({
        "CiphertextBlob": kms_key
    });

    let headers = build_get_secret_signed_headers(
        Arc::clone(&aws_config),
        aws_credentials,
        aws_config.region.clone(),
        RequestArgs {
            service: "kms".to_string(),
            body: json_body,
            time: Utc::now(),
            x_amz_target: "TrentService.Decrypt".to_string(),
        },
    );

    let v = request(json_body, headers?, client).await?;

    if let Some(secret_string_b64) = v["Plaintext"].as_str() {
        let secret_string = String::from_utf8(BASE64_STANDARD.decode(secret_string_b64)?)?;
        Ok(secret_string)
    } else {
        let json_body = &serde_json::json!({
            "CiphertextBlob": kms_key,
            "encryptionContext": { "LambdaFunctionName": aws_config.function_name }
        });

        let headers = build_get_secret_signed_headers(
            Arc::clone(&aws_config),
            aws_credentials,
            aws_config.region.clone(),
            RequestArgs {
                service: "kms".to_string(),
                body: json_body,
                time: Utc::now(),
                x_amz_target: "TrentService.Decrypt".to_string(),
            },
        );

        let v = request(json_body, headers?, client).await?;

        if let Some(secret_string_b64) = v["Plaintext"].as_str() {
            let secret_string = String::from_utf8(BASE64_STANDARD.decode(secret_string_b64)?)?;
            Ok(secret_string)
        } else {
            Err(Error::new(std::io::ErrorKind::InvalidData, v.to_string()).into())
        }
    }
}

async fn decrypt_aws_sm(
    client: &Client,
    secret_arn: String,
    aws_config: Arc<AwsConfig>,
    aws_credentials: &AwsCredentials,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let json_body = &serde_json::json!({ "SecretId": secret_arn});
    // Supports cross-region secrets
    let secret_region = secret_arn
        .split(':')
        .nth(3)
        .unwrap_or(&aws_config.region)
        .to_string();
    let headers = build_get_secret_signed_headers(
        aws_config,
        aws_credentials,
        secret_region,
        RequestArgs {
            service: "secretsmanager".to_string(),
            body: json_body,
            time: Utc::now(),
            x_amz_target: "secretsmanager.GetSecretValue".to_string(),
        },
    );

    let v = request(json_body, headers?, client).await?;

    if let Some(secret_string) = v["SecretString"].as_str() {
        Ok(secret_string.to_string())
    } else {
        Err(Error::new(std::io::ErrorKind::InvalidData, v.to_string()).into())
    }
}

async fn decrypt_aws_ssm(
    client: &Client,
    parameter_arn: String,
    aws_config: Arc<AwsConfig>,
    aws_credentials: &AwsCredentials,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let json_body = &serde_json::json!({ "Name": parameter_arn, "WithDecryption": true });
    let parameter_region = parameter_arn
        .split(':')
        .nth(3)
        .unwrap_or(&aws_config.region)
        .to_string();
    let headers = build_get_secret_signed_headers(
        aws_config,
        aws_credentials,
        parameter_region,
        RequestArgs {
            service: "ssm".to_string(),
            body: json_body,
            time: Utc::now(),
            x_amz_target: "AmazonSSM.GetParameter".to_string(),
        },
    );

    let v = request(json_body, headers?, client).await?;
    if let Some(parameter) = v["Parameter"].as_object()
        && let Some(value) = parameter["Value"].as_str()
    {
        return Ok(value.to_string());
    }
    Err(Error::new(std::io::ErrorKind::InvalidData, v.to_string()).into())
}

async fn get_snapstart_credentials(
    aws_credentials: &AwsCredentials,
    client: &Client,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&aws_credentials.aws_container_authorization_token)?,
    );

    let req = client
        .get(&aws_credentials.aws_container_credentials_full_uri)
        .headers(headers);
    let body = req.send().await?.text().await?;
    let v: Value = serde_json::from_str(&body)?;
    Ok(v)
}

async fn request(
    json_body: &Value,
    headers: HeaderMap,
    client: &Client,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let host_header = &headers["host"]
        .to_str()
        .map_err(|err| Error::new(std::io::ErrorKind::InvalidInput, err.to_string()))?;
    let req = client
        .post(format!("https://{host_header}"))
        .json(json_body)
        .headers(headers);

    let body = req.send().await?.text().await?;
    let v: Value = serde_json::from_str(&body)?;
    Ok(v)
}

fn build_get_secret_signed_headers(
    aws_config: Arc<AwsConfig>,
    aws_credentials: &AwsCredentials,
    region: String,
    header_values: RequestArgs,
) -> Result<HeaderMap, Box<dyn std::error::Error + Send + Sync>> {
    let amz_date = header_values.time.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = header_values.time.format("%Y%m%d").to_string();

    let domain = if aws_config.region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };

    let host = compute_aws_api_host(&header_values.service, &region, domain);

    let canonical_uri = "/";
    let canonical_querystring = "";
    let canonical_headers = format!(
        "content-type:application/x-amz-json-1.1\nhost:{}\nx-amz-date:{}\nx-amz-security-token:{}\nx-amz-target:{}",
        host, amz_date, aws_credentials.aws_session_token, header_values.x_amz_target
    );
    let signed_headers = "content-type;host;x-amz-date;x-amz-security-token;x-amz-target";

    let payload_hash = Sha256::digest(header_values.body.to_string().as_bytes());
    let payload_hash_hex = hex::encode(payload_hash);

    let canonical_request = format!(
        "POST\n{canonical_uri}\n{canonical_querystring}\n{canonical_headers}\n\n{signed_headers}\n{payload_hash_hex}"
    );
    let algorithm = "AWS4-HMAC-SHA256";
    let credential_scope = format!(
        "{}/{}/{}/aws4_request",
        date_stamp, region, header_values.service
    );
    let string_to_sign = format!(
        "{}\n{}\n{}\n{}",
        algorithm,
        amz_date,
        credential_scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let signing_key = get_aws4_signature_key(
        &aws_credentials.aws_secret_access_key,
        &date_stamp,
        region.as_str(),
        header_values.service.as_str(),
    )?;

    let signature = hex::encode(sign(&signing_key, &string_to_sign)?);

    let authorization_header = format!(
        "{} Credential={}/{}, SignedHeaders={}, Signature={}",
        algorithm, aws_credentials.aws_access_key_id, credential_scope, signed_headers, signature
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&authorization_header)?,
    );
    headers.insert("host", HeaderValue::from_str(&host)?);
    headers.insert(
        "Content-Type",
        HeaderValue::from_str("application/x-amz-json-1.1")?,
    );
    headers.insert("x-amz-date", HeaderValue::from_str(&amz_date)?);
    headers.insert(
        "x-amz-target",
        HeaderValue::from_str(header_values.x_amz_target.as_str())?,
    );
    headers.insert(
        "x-amz-security-token",
        HeaderValue::from_str(&aws_credentials.aws_session_token)?,
    );
    Ok(headers)
}

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
    use chrono::{NaiveDateTime, TimeZone};

    #[test]
    fn key_cleanup() {
        let key = clean_api_key(Some(" 32alxcxf\n".to_string()));
        assert_eq!(key.expect("it should parse the key"), "32alxcxf");
        let key = clean_api_key(Some("   \n".to_string()));
        assert_eq!(key, None);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_build_get_secret_signed_headers() {
        let time = Utc.from_utc_datetime(
            &NaiveDateTime::parse_from_str("2024-05-30 09:10:11", "%Y-%m-%d %H:%M:%S").unwrap(),
        );
        let headers = build_get_secret_signed_headers(
            Arc::new(AwsConfig {
                region: "us-east-1".to_string(),
                aws_lwa_proxy_lambda_runtime_api: Some("***".into()),
                function_name: "arn:some-function".to_string(),
                sandbox_init_time: Instant::now(),
                runtime_api: String::new(),
                exec_wrapper: None,
                initialization_type: "on-demand".into(),
            }),
            &AwsCredentials{
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
                aws_container_authorization_token: String::new(),
                aws_container_credentials_full_uri: String::new(),
            },
            "us-east-1".to_string(),
            RequestArgs {
                service: "secretsmanager".to_string(),
                body: &serde_json::json!({ "SecretId": "arn:aws:secretsmanager:region:account-id:secret:secret-name"}),
                time,
                x_amz_target: "secretsmanager.GetSecretValue".to_string(),
            },
        ).unwrap();

        let mut expected_headers = HeaderMap::new();
        expected_headers.insert("authorization", HeaderValue::from_str("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240530/us-east-1/secretsmanager/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature=63d50106f9c0ab1f02c1f81d6c720b01bce369d45f63a8f6280ffe7945405b81").unwrap());
        expected_headers.insert(
            "host",
            HeaderValue::from_str("secretsmanager.us-east-1.amazonaws.com").unwrap(),
        );
        expected_headers.insert(
            "content-type",
            HeaderValue::from_str("application/x-amz-json-1.1").unwrap(),
        );
        expected_headers.insert(
            "x-amz-date",
            HeaderValue::from_str("20240530T091011Z").unwrap(),
        );
        expected_headers.insert(
            "x-amz-target",
            HeaderValue::from_str("secretsmanager.GetSecretValue").unwrap(),
        );
        expected_headers.insert(
            "x-amz-security-token",
            HeaderValue::from_str("AQoDYXdzEJr...<remainder of session token>").unwrap(),
        );

        for (k, v) in &expected_headers {
            assert_eq!(headers.get(k).expect("cannot get header"), v);
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cross_region_secret() {
        let time = Utc.from_utc_datetime(
            &NaiveDateTime::parse_from_str("2024-05-30 09:10:11", "%Y-%m-%d %H:%M:%S").unwrap(),
        );
        let headers = build_get_secret_signed_headers(
            Arc::new(AwsConfig {
                region: "us-east-1".to_string(),
                aws_lwa_proxy_lambda_runtime_api: Some("***".into()),
                function_name: "arn:some-function".to_string(),
                sandbox_init_time: Instant::now(),
                runtime_api: String::new(),
                exec_wrapper: None,
                initialization_type: "on-demand".into(),
            }),
            &AwsCredentials{
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
                aws_container_authorization_token: String::new(),
                aws_container_credentials_full_uri: String::new(),
            },
            "us-west-2".to_string(),
            RequestArgs {
                service: "secretsmanager".to_string(),
                body: &serde_json::json!({ "SecretId": "arn:aws:secretsmanager:us-west-2:account-id:secret:secret-name"}),
                time,
                x_amz_target: "secretsmanager.GetSecretValue".to_string(),
            },
        ).unwrap();

        let mut expected_headers = HeaderMap::new();
        expected_headers.insert("authorization", HeaderValue::from_str("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240530/us-west-2/secretsmanager/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature=bd77112dd24d7a35566b29a39861f7421660c49b065964212f966db509e90813").unwrap());
        expected_headers.insert(
            "host",
            HeaderValue::from_str("secretsmanager.us-west-2.amazonaws.com").unwrap(),
        );
        expected_headers.insert(
            "content-type",
            HeaderValue::from_str("application/x-amz-json-1.1").unwrap(),
        );
        expected_headers.insert(
            "x-amz-date",
            HeaderValue::from_str("20240530T091011Z").unwrap(),
        );
        expected_headers.insert(
            "x-amz-target",
            HeaderValue::from_str("secretsmanager.GetSecretValue").unwrap(),
        );
        expected_headers.insert(
            "x-amz-security-token",
            HeaderValue::from_str("AQoDYXdzEJr...<remainder of session token>").unwrap(),
        );

        for (k, v) in &expected_headers {
            assert_eq!(headers.get(k).expect("cannot get header"), v);
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_ssm_parameter_headers() {
        let time = Utc.from_utc_datetime(
            &NaiveDateTime::parse_from_str("2024-05-30 09:10:11", "%Y-%m-%d %H:%M:%S").unwrap(),
        );
        let headers = build_get_secret_signed_headers(
            Arc::new(AwsConfig {
                region: "us-east-1".to_string(),
                aws_lwa_proxy_lambda_runtime_api: Some("***".into()),
                function_name: "arn:some-function".to_string(),
                sandbox_init_time: Instant::now(),
                runtime_api: String::new(),
                exec_wrapper: None,
                initialization_type: "on-demand".into(),
            }),
            &AwsCredentials{
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
                aws_container_authorization_token: String::new(),
                aws_container_credentials_full_uri: String::new(),
            },
            "us-east-1".to_string(),
            RequestArgs {
                service: "ssm".to_string(),
                body: &serde_json::json!({ "Name": "arn:aws:ssm:us-east-1:account-id:parameter/my-parameter", "WithDecryption": true }),
                time,
                x_amz_target: "AmazonSSM.GetParameter".to_string(),
            },
        ).unwrap();

        let mut expected_headers = HeaderMap::new();
        expected_headers.insert("authorization", HeaderValue::from_str("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240530/us-east-1/ssm/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature=f45eebacb9f2f575cf1a235c65c97adc1c0d6ca21174044baa0c6457d4518d64").unwrap());
        expected_headers.insert(
            "host",
            HeaderValue::from_str("ssm.us-east-1.amazonaws.com").unwrap(),
        );
        expected_headers.insert(
            "content-type",
            HeaderValue::from_str("application/x-amz-json-1.1").unwrap(),
        );
        expected_headers.insert(
            "x-amz-date",
            HeaderValue::from_str("20240530T091011Z").unwrap(),
        );
        expected_headers.insert(
            "x-amz-target",
            HeaderValue::from_str("AmazonSSM.GetParameter").unwrap(),
        );
        expected_headers.insert(
            "x-amz-security-token",
            HeaderValue::from_str("AQoDYXdzEJr...<remainder of session token>").unwrap(),
        );

        for (k, v) in &expected_headers {
            assert_eq!(headers.get(k).expect("cannot get header"), v);
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cross_region_ssm_parameter() {
        let time = Utc.from_utc_datetime(
            &NaiveDateTime::parse_from_str("2024-05-30 09:10:11", "%Y-%m-%d %H:%M:%S").unwrap(),
        );
        let headers = build_get_secret_signed_headers(
            Arc::new(AwsConfig {
                region: "us-east-1".to_string(),
                aws_lwa_proxy_lambda_runtime_api: Some("***".into()),
                function_name: "arn:some-function".to_string(),
                sandbox_init_time: Instant::now(),
                runtime_api: String::new(),
                exec_wrapper: None,
                initialization_type: "on-demand".into(),
            }),
            &AwsCredentials{
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
                aws_container_authorization_token: String::new(),
                aws_container_credentials_full_uri: String::new(),
            },
            "eu-west-1".to_string(),
            RequestArgs {
                service: "ssm".to_string(),
                body: &serde_json::json!({ "Name": "arn:aws:ssm:eu-west-1:account-id:parameter/my-parameter", "WithDecryption": true }),
                time,
                x_amz_target: "AmazonSSM.GetParameter".to_string(),
            },
        ).unwrap();

        let mut expected_headers = HeaderMap::new();
        expected_headers.insert("authorization", HeaderValue::from_str("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240530/eu-west-1/ssm/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token;x-amz-target, Signature=4bdd2a27b632981c73a70b52ea84e92d3e26fe2c768e9cf18b96630188748b7a").unwrap());
        expected_headers.insert(
            "host",
            HeaderValue::from_str("ssm.eu-west-1.amazonaws.com").unwrap(),
        );
        expected_headers.insert(
            "content-type",
            HeaderValue::from_str("application/x-amz-json-1.1").unwrap(),
        );
        expected_headers.insert(
            "x-amz-date",
            HeaderValue::from_str("20240530T091011Z").unwrap(),
        );
        expected_headers.insert(
            "x-amz-target",
            HeaderValue::from_str("AmazonSSM.GetParameter").unwrap(),
        );
        expected_headers.insert(
            "x-amz-security-token",
            HeaderValue::from_str("AQoDYXdzEJr...<remainder of session token>").unwrap(),
        );

        for (k, v) in &expected_headers {
            assert_eq!(headers.get(k).expect("cannot get header"), v);
        }
    }
}
