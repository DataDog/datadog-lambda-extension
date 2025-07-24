use std::collections::HashMap;

use super::{ExtractResponse, HttpData, body::parse_body};

use aws_lambda_events::{alb, apigw, lambda_function_urls};
use tracing::warn;

impl ExtractResponse for apigw::ApiGatewayProxyResponse {
    async fn extract(self) -> HttpData {
        let body = if let Some(body) = self.body {
            match parse_body(
                body,
                self.is_base64_encoded,
                self.headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
            )
            .await
            {
                Ok(body) => body,
                Err(e) => {
                    warn!("appsec: failed to parse response body: {e}");
                    None
                }
            }
        } else {
            None
        };

        HttpData::Response {
            status_code: Some(self.status_code),
            headers: normalize_headers(self.multi_value_headers, self.headers),
            body,
        }
    }
}

impl ExtractResponse for apigw::ApiGatewayV2httpResponse {
    async fn extract(self) -> HttpData {
        let body = if let Some(body) = self.body {
            match parse_body(
                body,
                self.is_base64_encoded,
                self.headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
            )
            .await
            {
                Ok(body) => body,
                Err(e) => {
                    warn!("appsec: failed to parse response body: {e}");
                    None
                }
            }
        } else {
            None
        };

        HttpData::Response {
            status_code: Some(self.status_code),
            headers: normalize_headers(self.multi_value_headers, self.headers),
            body,
        }
    }
}

/// Used for response payloads from which no data can be readily extracted.
#[derive(serde::Deserialize)]
#[repr(transparent)]
pub(super) struct Opaque(serde_json::Value);
impl ExtractResponse for Opaque {
    async fn extract(self) -> HttpData {
        HttpData::Response {
            status_code: None,
            headers: None,
            body: None,
        }
    }
}

impl ExtractResponse for alb::AlbTargetGroupResponse {
    async fn extract(self) -> HttpData {
        let body = if let Some(body) = self.body {
            match parse_body(
                body,
                self.is_base64_encoded,
                self.headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
            )
            .await
            {
                Ok(body) => body,
                Err(e) => {
                    warn!("appsec: failed to parse response body: {e}");
                    None
                }
            }
        } else {
            None
        };

        HttpData::Response {
            status_code: Some(self.status_code),
            headers: normalize_headers(self.multi_value_headers, self.headers),
            body,
        }
    }
}

impl ExtractResponse for lambda_function_urls::LambdaFunctionUrlResponse {
    async fn extract(self) -> HttpData {
        let body = if let Some(body) = self.body {
            match parse_body(
                body,
                self.is_base64_encoded,
                self.headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
            )
            .await
            {
                Ok(body) => body,
                Err(e) => {
                    warn!("appsec: failed to parse response body: {e}");
                    None
                }
            }
        } else {
            None
        };

        HttpData::Response {
            status_code: Some(self.status_code),
            headers: normalize_headers(aws_lambda_events::http::HeaderMap::default(), self.headers),
            body,
        }
    }
}

/// Converts a header multimap + single-map into a normalized multi-map where all header names are
/// normalized to the lower case form. It also removes the `Set-Cookie` header, which should not be
/// carried any further due to its potential for including sensitive information.
pub(super) fn normalize_headers(
    multi_value_headers: aws_lambda_events::http::HeaderMap,
    headers: aws_lambda_events::http::HeaderMap,
) -> Option<HashMap<String, Vec<String>>> {
    if multi_value_headers.is_empty() && headers.is_empty() {
        return None;
    }

    let mut normalized =
        HashMap::with_capacity(multi_value_headers.keys_len() + headers.keys_len());

    for key in multi_value_headers.keys() {
        let key = key.as_str();
        if key.eq_ignore_ascii_case("set-cookie") {
            continue;
        }

        normalized.insert(
            key.to_lowercase(),
            multi_value_headers
                .get_all(key)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .map(str::to_string)
                .collect(),
        );
    }

    for key in headers.keys() {
        let key = key.as_str();
        if key.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let Some(value) = headers.get(key).and_then(|v| v.to_str().ok()) else {
            continue;
        };
        normalized
            .entry(key.to_lowercase())
            .or_insert_with(move || vec![value.to_string()]);
    }

    Some(normalized)
}
