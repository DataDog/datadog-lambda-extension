//! Minimal SigV4-signed S3 `PutObject` for recording segments.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fmt::Write;

#[derive(Clone)]
pub struct S3Uploader {
    pub bucket: String,
    pub prefix: String,
    pub region: String,
    client: reqwest::Client,
}

struct Credentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl S3Uploader {
    #[must_use]
    pub fn new(bucket: String, prefix: String, region: String, client: reqwest::Client) -> Self {
        Self {
            bucket,
            prefix: prefix.trim_matches('/').to_string(),
            region,
            client,
        }
    }

    pub async fn put(&self, key: &str, body: Vec<u8>) -> Result<(), String> {
        let creds = Credentials {
            access_key: std::env::var("AWS_ACCESS_KEY_ID").map_err(|_| "no AWS_ACCESS_KEY_ID")?,
            secret_key: std::env::var("AWS_SECRET_ACCESS_KEY")
                .map_err(|_| "no AWS_SECRET_ACCESS_KEY")?,
            session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
        };
        let key = if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}/{key}", self.prefix)
        };
        let host = format!("{}.s3.{}.amazonaws.com", self.bucket, self.region);
        let path = format!("/{}", encode_path(&key));
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let headers = sign_put(&creds, &self.region, &host, &path, &amz_date, &body);

        let mut rb = self.client.put(format!("https://{host}{path}"));
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        let resp = rb.body(body).send().await.map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            Err(format!(
                "{status}: {}",
                resp.text().await.unwrap_or_default()
            ))
        }
    }
}

fn encode_path(key: &str) -> String {
    key.split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn hmac(key: &[u8], msg: &str) -> Vec<u8> {
    #[allow(clippy::expect_used)]
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(msg.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn sign_put(
    creds: &Credentials,
    region: &str,
    host: &str,
    path: &str,
    amz_date: &str,
    body: &[u8],
) -> Vec<(String, String)> {
    let date = &amz_date[..8];
    let payload_hash = hex::encode(Sha256::digest(body));
    let mut headers = vec![
        ("host".to_string(), host.to_string()),
        ("x-amz-content-sha256".to_string(), payload_hash.clone()),
        ("x-amz-date".to_string(), amz_date.to_string()),
    ];
    if let Some(t) = &creds.session_token {
        headers.push(("x-amz-security-token".to_string(), t.clone()));
    }
    let canonical_headers = headers.iter().fold(String::new(), |mut acc, (k, v)| {
        let _ = writeln!(acc, "{k}:{v}");
        acc
    });
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request =
        format!("PUT\n{path}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let k_date = hmac(format!("AWS4{}", creds.secret_key).as_bytes(), date);
    let k_region = hmac(&k_date, region);
    let k_service = hmac(&k_region, "s3");
    let k_signing = hmac(&k_service, "aws4_request");
    let signature = hex::encode(hmac(&k_signing, &string_to_sign));
    headers.push((
        "authorization".to_string(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            creds.access_key
        ),
    ));
    // reqwest sets Host from the URL.
    headers.retain(|(k, _)| k != "host");
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_encoding_keeps_slashes() {
        assert_eq!(encode_path("fn/a b/0001.jsonl"), "fn/a%20b/0001.jsonl");
    }

    #[test]
    fn signature_is_stable() {
        let creds = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let a = sign_put(
            &creds,
            "us-east-1",
            "b.s3.us-east-1.amazonaws.com",
            "/k",
            "20260101T000000Z",
            b"x",
        );
        let b = sign_put(
            &creds,
            "us-east-1",
            "b.s3.us-east-1.amazonaws.com",
            "/k",
            "20260101T000000Z",
            b"x",
        );
        assert_eq!(a, b);
        let auth = &a
            .iter()
            .find(|(k, _)| k == "authorization")
            .expect("auth")
            .1;
        assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
    }
}
