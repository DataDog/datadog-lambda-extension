use crate::config::Config;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::env;
use std::io::{Error, Result};
use std::time::Instant;
use tracing::debug;

pub fn resolve_secrets(config: Config) -> Result<Config> {
    if !config.api_key.is_empty() {
        debug!("DD_API_KEY found, not trying to resolve secrets");
        Ok(config)
    } else if !config.api_key_secret_arn.is_empty() {
        let before_manual = Instant::now();

        let resolved_key = manual_decrypt(
            config.api_key_secret_arn.clone(),
            AwsConfig {
                region: env::var("AWS_DEFAULT_REGION").expect("AWS_DEFAULT_REGION not set"),
                aws_access_key_id: env::var("AWS_ACCESS_KEY_ID")
                    .expect("AWS_ACCESS_KEY_ID not set"),
                aws_secret_access_key: env::var("AWS_SECRET_ACCESS_KEY")
                    .expect("AWS_SECRET_ACCESS_KEY not set"),
                aws_session_token: env::var("AWS_SESSION_TOKEN")
                    .expect("AWS_SESSION_TOKEN is not set!"),
            },
        )
        .expect("Failed to decrypt secret");
        debug!("AWS decrypt took {}ms", before_manual.elapsed().as_millis());

        Ok(Config {
            api_key: resolved_key,
            ..config.clone()
        })
    } else {
        Err(Error::new(
            std::io::ErrorKind::InvalidInput,
            "No API key or secret ARN found".to_string(),
        ))
    }
}

struct AwsConfig {
    region: String,
    aws_access_key_id: String,
    aws_secret_access_key: String,
    aws_session_token: String,
}

fn manual_decrypt(secret_arn: String, aws_config: AwsConfig) -> Result<String> {
    let json_body = &serde_json::json!({ "SecretId": secret_arn});

    let headers = build_get_secret_signed_headers(&aws_config, json_body, Utc::now());

    let client = reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .build()
        .expect("Failed to create reqwest client for aws decrypt");

    let req = client
        .post(format!(
            "https://{}",
            &headers["host"].to_str().expect("invalid host")
        ))
        .json(json_body)
        .headers(headers);

    let resp = req.send();
    let body = resp
        .expect("Failed to get response body")
        .text()
        .expect("Cannot deserialize body");
    let v: Value = serde_json::from_str(&body).expect("Failed to parse JSON");

    return if let Some(secret_string) = v["SecretString"].as_str() {
        Ok(secret_string.to_string())
    } else {
        Err(Error::new(std::io::ErrorKind::InvalidData, v.to_string()))
    };
}

fn build_get_secret_signed_headers(
    aws_config: &AwsConfig,
    json_body: &Value,
    t: DateTime<Utc>,
) -> HeaderMap {
    let service = "secretsmanager";
    let amz_date = t.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = t.format("%Y%m%d").to_string();
    let host = format!("{}.{}.amazonaws.com", service, aws_config.region);

    let canonical_uri = "/";
    let canonical_querystring = "";
    let canonical_headers = format!(
        "content-type:application/x-amz-json-1.1\nhost:{}\nx-amz-date:{}\nx-amz-security-token:{}\nx-amz-target:secretsmanager.GetSecretValue",
        host, amz_date, aws_config.aws_session_token);
    let signed_headers = "content-type;host;x-amz-date;x-amz-security-token;x-amz-target";

    let payload_hash = Sha256::digest(json_body.to_string().as_bytes());
    let payload_hash_hex = hex::encode(payload_hash);

    let canonical_request = format!(
        "POST\n{canonical_uri}\n{canonical_querystring}\n{canonical_headers}\n\n{signed_headers}\n{payload_hash_hex}"
    );
    let algorithm = "AWS4-HMAC-SHA256";
    let credential_scope = format!(
        "{}/{}/{}/aws4_request",
        date_stamp, aws_config.region, service
    );
    let string_to_sign = format!(
        "{}\n{}\n{}\n{}",
        algorithm,
        amz_date,
        credential_scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let signing_key = get_aws4_signature_key(
        &aws_config.aws_secret_access_key,
        &date_stamp,
        aws_config.region.as_str(),
        service,
    );

    let signature = hex::encode(sign(&signing_key, &string_to_sign));

    let authorization_header = format!(
        "{} Credential={}/{}, SignedHeaders={}, Signature={}",
        algorithm, aws_config.aws_access_key_id, credential_scope, signed_headers, signature
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&authorization_header).expect("invalid authorization"),
    );
    headers.insert("host", HeaderValue::from_str(&host).expect("invalid host"));
    headers.insert(
        "Content-Type",
        HeaderValue::from_str("application/x-amz-json-1.1").expect("invalid content-type"),
    );
    headers.insert(
        "x-amz-date",
        HeaderValue::from_str(&amz_date).expect("invalid x-amz-date"),
    );
    headers.insert(
        "x-amz-target",
        HeaderValue::from_str("secretsmanager.GetSecretValue").expect("invalid x-amz-target"),
    );
    headers.insert(
        "x-amz-security-token",
        HeaderValue::from_str(&aws_config.aws_session_token).expect("invalid x-amz-security-token"),
    );
    headers
}

fn sign(key: &[u8], msg: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(msg.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn get_aws4_signature_key(
    key: &str,
    date_stamp: &str,
    region_name: &str,
    service_name: &str,
) -> Vec<u8> {
    let k_date = sign(format!("AWS4{key}").as_bytes(), date_stamp);
    let k_region = sign(&k_date, region_name);
    let k_service = sign(&k_region, service_name);
    sign(&k_service, "aws4_request")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDateTime, TimeZone};

    #[test]
    fn test_build_get_secret_signed_headers() {
        let time = Utc.from_utc_datetime(
            &NaiveDateTime::parse_from_str("2024-05-30 09:10:11", "%Y-%m-%d %H:%M:%S").unwrap(),
        );
        let headers = build_get_secret_signed_headers(
            &AwsConfig {
                region: "us-east-1".to_string(),
                aws_access_key_id: "AKIDEXAMPLE".to_string(),
                aws_secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                aws_session_token: "AQoDYXdzEJr...<remainder of session token>".to_string(),
            },
            &serde_json::json!({ "SecretId": "arn:aws:secretsmanager:region:account-id:secret:secret-name"}),
            time,
        );

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
            assert_eq!(headers.get(k).unwrap(), v);
        }
    }
}