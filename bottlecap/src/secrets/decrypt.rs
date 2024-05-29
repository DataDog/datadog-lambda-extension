use std::env;
use std::io::Error;
use crate::config::Config;
use tracing::debug;
use std::time::Instant;

use chrono::{Utc};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

type ResolveFn = fn(String) -> Result<String, Error>;

pub fn resolve_secrets(config: Config) -> Result<Config, String> {
    if !config.api_key.is_empty() {
        debug!("DD_API_KEY found, not trying to resolve secrets");
        Ok(config)
    } else {
        if !config.api_key_secret_arn.is_empty() {
            let config_clone = config.clone();
            debug!("DD_API_KEY_SECRET_ARN found, trying to resolve ARN secret");
            let string = config.api_key_secret_arn.clone();

            let before_manual = Instant::now();
            let manual_res = manual_decrypt(config.api_key_secret_arn);
            let duration_manual = before_manual.elapsed();
            println!("Manual took {}ms", duration_manual.as_millis());

            return Ok(Config {
                api_key: manual_res,
                ..config_clone.clone()
            });
        } else {
            Err("No API key or secret ARN found".to_string())
        }
    }
}


type HmacSha256 = Hmac<Sha256>;

fn sign(key: &[u8], msg: &str) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(msg.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn get_signature_key(key: &str, date_stamp: &str, region_name: &str, service_name: &str) -> Vec<u8> {
    let k_date = sign(format!("AWS4{}", key).as_bytes(), date_stamp);
    let k_region = sign(&k_date, region_name);
    let k_service = sign(&k_region, service_name);
    sign(&k_service, "aws4_request")
}


fn manual_decrypt(secret_arn: String) -> String {
    let start = Instant::now();

    let region = "us-east-1";

    let access_key = env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID not set");
    let secret_key = env::var("AWS_SECRET_ACCESS_KEY").expect("AWS_SECRET_ACCESS_KEY not set");
    let session_token = env::var("AWS_SESSION_TOKEN").expect("AWS_SESSION_TOKEN is not set!");

    let service = "secretsmanager";
    let host = format!("{}.{}.amazonaws.com", service, region);
    let endpoint = format!("https://{}", host);

    let t = Utc::now();
    let amz_date = t.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = t.format("%Y%m%d").to_string();

    let canonical_uri = "/";
    let canonical_querystring = "";
    let canonical_headers = format!(
        "content-type:application/x-amz-json-1.1\nhost:{}\nx-amz-date:{}\nx-amz-security-token:{}\nx-amz-target:secretsmanager.GetSecretValue",
        host, amz_date, session_token);
    let signed_headers = "content-type;host;x-amz-date;x-amz-security-token;x-amz-target";


    let step1 = start.elapsed();
    let start1 = Instant::now();

    let json_body = &serde_json::json!({ "SecretId": secret_arn});

    let step2 = start1.elapsed();
    let start2 = Instant::now();

    let payload_hash = Sha256::digest(json_body.to_string().as_bytes());
    let payload_hash_hex = hex::encode(payload_hash);

    let step3 = start2.elapsed();
    let start3 = Instant::now();

    let canonical_request = format!(
        "POST\n{}\n{}\n{}\n\n{}\n{}",
        canonical_uri, canonical_querystring, canonical_headers, signed_headers, payload_hash_hex
    );

    // println!("Canonical request: {:?}", canonical_request);

    let algorithm = "AWS4-HMAC-SHA256";
    let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, region, service);
    let string_to_sign = format!(
        "{}\n{}\n{}\n{}",
        algorithm,
        amz_date,
        credential_scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let signing_key = get_signature_key(&secret_key, &date_stamp, region, service);

    let signature = hex::encode(sign(&signing_key, &string_to_sign));

    // println!("Signature: {:?}", signature);

    let authorization_header = format!(
        "{} Credential={}/{}, SignedHeaders={}, Signature={}",
        algorithm, access_key, credential_scope, signed_headers, signature
    );

    let step4 = start3.elapsed();
    let start4 = Instant::now();

    let client = reqwest::blocking::Client::new();

    let step5 = start4.elapsed();
    let start5 = Instant::now();

    let mut headers = HeaderMap::new();
    headers.insert("Authorization", HeaderValue::from_str(&authorization_header).unwrap());
    headers.insert("host", HeaderValue::from_str(&host).unwrap());
    headers.insert("Content-Type", HeaderValue::from_str("application/x-amz-json-1.1").unwrap());
    headers.insert("x-amz-date", HeaderValue::from_str(&amz_date).unwrap());
    headers.insert("x-amz-target", HeaderValue::from_str("secretsmanager.GetSecretValue").unwrap());
    headers.insert("x-amz-security-token", HeaderValue::from_str(&session_token).unwrap());

    let step6 = start5.elapsed();
    let start6 = Instant::now();

    let req = client
        .post(&endpoint)
        .json(json_body)
        .headers(headers);

    // println!("Request: {:?}", req);

    let resp = req.send();

    // println!("Response: {:?}", resp);

    let body = resp.unwrap().text().unwrap();

    // println!("Body: {:?}", body);
    let step7 = start6.elapsed();
    let start7 = Instant::now();

    let v: serde_json::Value = serde_json::from_str(&body).expect("Failed to parse JSON");

    let step8 = start7.elapsed();

    println!("Steps took {}, {}, {}, {}, {}, {}, {}, {}",
             step1.as_millis(), step2.as_millis(), step3.as_millis(), step4.as_millis(), step5.as_millis(),
             step6.as_millis(), step7.as_millis(), step8.as_millis());

    let mut to_ret = "".to_string();
    if let Some(secret_string) = v["SecretString"].as_str() {
        to_ret = secret_string.to_string();
    }
    return to_ret;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn immediate_return(secret_arn: String) -> Result<String, Error> {
        Ok(secret_arn)
    }

    #[test]
    fn test_resolve_secrets_sync() {
        let secret_arn = "arn:aws:secretsmanager:region:account-id:secret:secret-name".to_string();

        let result = match resolve_secrets(
            Config {
                api_key_secret_arn: secret_arn.clone(),
                ..Config::default()
            }, immediate_return) {
            Ok(config) => config.api_key,
            Err(e) => panic!("{}", e)
        };

        assert_eq!(result, secret_arn);
    }
}
