// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use hyper::{
    header,
    http::{self, HeaderMap},
    Body, Response, StatusCode,
};
use log::{error, info};
use serde_json::json;

/// Does two things:
/// 1. Logs the given message. A success status code (within 200-299) will cause an info log to be
///    written,
/// otherwise error will be written.
/// 2. Returns the given message in the body of JSON response with the given status code.
///
/// Response body format:
/// {
///     "message": message
/// }
pub fn log_and_create_http_response(
    message: &str,
    status: StatusCode,
) -> http::Result<Response<Body>> {
    if status.is_success() {
        info!("{message}");
    } else {
        error!("{message}");
    }
    let body = json!({ "message": message }).to_string();
    Response::builder().status(status).body(Body::from(body))
}

/// Takes a request's header map, and verifies that the "content-length" header is present, valid,
/// and less than the given max_content_length.
///
/// Will return None if no issues are found. Otherwise logs an error (with the given prefix) and
/// returns and HTTP Response with the appropriate error status code.
pub fn verify_request_content_length(
    header_map: &HeaderMap,
    max_content_length: usize,
    error_message_prefix: &str,
) -> Option<http::Result<Response<Body>>> {
    let content_length_header = match header_map.get(header::CONTENT_LENGTH) {
        Some(res) => res,
        None => {
            return Some(log_and_create_http_response(
                &format!("{error_message_prefix}: Missing Content-Length header"),
                StatusCode::LENGTH_REQUIRED,
            ));
        }
    };
    let header_as_string = match content_length_header.to_str() {
        Ok(res) => res,
        Err(_) => {
            return Some(log_and_create_http_response(
                &format!("{error_message_prefix}: Invalid Content-Length header"),
                StatusCode::BAD_REQUEST,
            ));
        }
    };
    let content_length = match header_as_string.to_string().parse::<usize>() {
        Ok(res) => res,
        Err(_) => {
            return Some(log_and_create_http_response(
                &format!("{error_message_prefix}: Invalid Content-Length header"),
                StatusCode::BAD_REQUEST,
            ));
        }
    };
    if content_length > max_content_length {
        return Some(log_and_create_http_response(
            &format!("{error_message_prefix}: Payload too large"),
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use hyper::header;
    use hyper::Body;
    use hyper::HeaderMap;
    use hyper::Response;
    use hyper::StatusCode;

    use super::verify_request_content_length;

    fn create_test_headers_with_content_length(val: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(header::CONTENT_LENGTH, val.parse().unwrap());
        map
    }

    async fn get_response_body_as_string(response: Response<Body>) -> String {
        let body = response.into_body();
        let bytes = hyper::body::to_bytes(body).await.unwrap();
        String::from_utf8(bytes.into_iter().collect()).unwrap()
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_request_content_length_missing() {
        let verify_result = verify_request_content_length(&HeaderMap::new(), 1, "Test Prefix");
        assert!(verify_result.is_some());

        let response = verify_result.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::LENGTH_REQUIRED);
        assert_eq!(
            get_response_body_as_string(response).await,
            "{\"message\":\"Test Prefix: Missing Content-Length header\"}".to_string()
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_request_content_length_cant_convert_to_str() {
        let verify_result = verify_request_content_length(
            &create_test_headers_with_content_length("❤❤❤❤❤❤❤"),
            1,
            "Test Prefix",
        );
        assert!(verify_result.is_some());

        let response = verify_result.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            get_response_body_as_string(response).await,
            "{\"message\":\"Test Prefix: Invalid Content-Length header\"}".to_string()
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_request_content_length_cant_convert_to_usize() {
        let verify_result = verify_request_content_length(
            &create_test_headers_with_content_length("not_an_int"),
            1,
            "Test Prefix",
        );
        assert!(verify_result.is_some());

        let response = verify_result.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            get_response_body_as_string(response).await,
            "{\"message\":\"Test Prefix: Invalid Content-Length header\"}".to_string()
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_request_content_length_too_long() {
        let verify_result = verify_request_content_length(
            &create_test_headers_with_content_length("100"),
            1,
            "Test Prefix",
        );

        assert!(verify_result.is_some());

        let response = verify_result.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            get_response_body_as_string(response).await,
            "{\"message\":\"Test Prefix: Payload too large\"}".to_string()
        );
    }
}
