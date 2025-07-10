use super::{body::parse_body, ExtractResponse, HttpRequestData};

use aws_lambda_events::{alb, apigw, lambda_function_urls};
use tracing::warn;

impl ExtractResponse for apigw::ApiGatewayProxyResponse {
    async fn extract(self) -> HttpRequestData {
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

        HttpRequestData {
            response_status: Some(self.status_code),
            response_body: body,
            ..Default::default()
        }
    }
}

impl ExtractResponse for apigw::ApiGatewayV2httpResponse {
    async fn extract(self) -> HttpRequestData {
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

        HttpRequestData {
            response_status: Some(self.status_code),
            response_body: body,
            ..Default::default()
        }
    }
}

/// Used for response payloads from which no data can be readily extracted.
#[derive(serde::Deserialize)]
#[repr(transparent)]
pub(super) struct Opaque(serde_json::Value);
impl ExtractResponse for Opaque {
    async fn extract(self) -> HttpRequestData {
        HttpRequestData::default()
    }
}

impl ExtractResponse for alb::AlbTargetGroupResponse {
    async fn extract(self) -> HttpRequestData {
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

        HttpRequestData {
            response_status: Some(self.status_code),
            response_body: body,
            ..Default::default()
        }
    }
}

impl ExtractResponse for lambda_function_urls::LambdaFunctionUrlResponse {
    async fn extract(self) -> HttpRequestData {
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

        HttpRequestData {
            response_status: Some(self.status_code),
            response_body: body,
            ..Default::default()
        }
    }
}
