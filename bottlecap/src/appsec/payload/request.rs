use super::{body::parse_body, ExtractRequest, HttpData, IsValid, RequestType};

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use aws_lambda_events::{
    alb, apigw, cloudwatch_events, cloudwatch_logs, dynamodb, eventbridge, kinesis,
    lambda_function_urls, s3, sns, sqs,
};

/// Kong API Gateway events are a subset of [`apigw::ApiGatewayProxyRequest`].
#[derive(serde::Deserialize)]
pub(super) struct KongAPIGatewayEvent(apigw::ApiGatewayProxyRequest);

trait RecordSet {
    const RECORD_KEY: &'static str;
}
impl<T: RecordSet> IsValid for T {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("Records") {
            Some(serde_json::Value::Array(records)) => records.iter().any(|record| match record {
                serde_json::Value::Object(record) => {
                    matches!(
                        record.get(Self::RECORD_KEY),
                        Some(serde_json::Value::Object(_))
                    )
                }
                _ => false,
            }),
            _ => false,
        }
    }
}

impl IsValid for apigw::ApiGatewayProxyRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        let Some(serde_json::Value::Object(request_context)) = map.get("requestContext") else {
            return false;
        };
        matches!(
            request_context.get("stage"),
            Some(serde_json::Value::String(_))
        ) && matches!(map.get("httpMethod"), Some(serde_json::Value::String(_)))
            && matches!(map.get("resource"), Some(serde_json::Value::String(_)))
            && !apigw::ApiGatewayCustomAuthorizerRequestTypeRequest::is_valid(map)
    }
}
impl ExtractRequest for apigw::ApiGatewayProxyRequest {
    const TYPE: RequestType = RequestType::APIGatewayV1;
    async fn extract(self) -> HttpData {
        let (headers, cookies) = filter_headers(self.multi_value_headers);

        // Headers are normalized to lowercase by [`filter_headers`].
        let content_type = headers
            .get("content-type")
            .and_then(|v| v.first())
            .map(String::as_str);
        let body = if let Some(body) = self.body {
            parse_body(body, self.is_base64_encoded, content_type)
                .await
                .ok()
                .flatten()
        } else {
            None
        };

        HttpData::Request {
            raw_uri: self.path,
            method: Some(self.http_method.to_string()),
            route: self.resource,
            client_ip: self.request_context.identity.source_ip, // API Gateway exposes the Client IP as the Source IP
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.query_string_parameters),
            path_params: Some(self.path_parameters),
            body,
        }
    }
}
impl IsValid for apigw::ApiGatewayV2httpRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("version") {
            Some(serde_json::Value::String(version)) => {
                version == "2.0"
                    && matches!(
                        map.get("rawQueryString"),
                        Some(serde_json::Value::String(_))
                    )
                    && match map.get("requestContext") {
                        Some(serde_json::Value::Object(request_context)) => {
                            if let Some(serde_json::Value::String(domain_name)) =
                                request_context.get("domainName")
                            {
                                !domain_name.contains(".lambda-url.")
                            } else {
                                false
                            }
                        }
                        _ => false,
                    }
            }
            _ => false,
        }
    }
}
impl ExtractRequest for apigw::ApiGatewayV2httpRequest {
    const TYPE: RequestType = RequestType::APIGatewayV2Http;
    async fn extract(self) -> HttpData {
        let (headers, cookies) = filter_headers(self.headers);

        let content_type = headers
            .get("content-type")
            .and_then(|v| v.first())
            .map(String::as_str);
        let body = if let Some(body) = self.body {
            parse_body(body, self.is_base64_encoded, content_type)
                .await
                .ok()
                .flatten()
        } else {
            None
        };

        HttpData::Request {
            raw_uri: self.raw_path,
            route: self.route_key,
            method: Some(self.http_method.to_string()),
            client_ip: self.request_context.http.source_ip, // API Gateway exposes the Client IP as the Source IP
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.query_string_parameters),
            path_params: Some(self.path_parameters),
            body,
        }
    }
}
impl IsValid for KongAPIGatewayEvent {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        // NB -- This is checked last, so we no longer need to worry about it possibly being a custom authorizer request
        matches!(map.get("httpMethod"), Some(serde_json::Value::String(_)))
            && matches!(map.get("resource"), Some(serde_json::Value::String(_)))
    }
}
impl ExtractRequest for KongAPIGatewayEvent {
    const TYPE: RequestType = RequestType::APIGatewayV1;
    async fn extract(self) -> HttpData {
        self.0.extract().await
    }
}
impl IsValid for apigw::ApiGatewayWebsocketProxyRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("requestContext") {
            Some(serde_json::Value::Object(request_context)) => {
                matches!(
                    request_context.get("messageDirection"),
                    Some(serde_json::Value::String(_))
                )
            }
            _ => false,
        }
    }
}
impl ExtractRequest for apigw::ApiGatewayWebsocketProxyRequest {
    const TYPE: RequestType = RequestType::APIGatewayV2Websocket;
    async fn extract(self) -> HttpData {
        let (headers, cookies) = filter_headers(self.multi_value_headers);

        let content_type = headers
            .get("content-type")
            .and_then(|v| v.first())
            .map(String::as_str);
        let body = if let Some(body) = self.body {
            parse_body(body, self.is_base64_encoded, content_type)
                .await
                .ok()
                .flatten()
        } else {
            None
        };

        HttpData::Request {
            raw_uri: self.path,
            method: self.http_method.map(|m| m.to_string()),
            route: self.resource,
            client_ip: self.request_context.identity.source_ip, // API Gateway exposes the Client IP as the Source IP
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.multi_value_query_string_parameters),
            path_params: Some(self.path_parameters),
            body,
        }
    }
}
impl IsValid for apigw::ApiGatewayCustomAuthorizerRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("type") {
            Some(serde_json::Value::String(t)) => {
                t == "TOKEN"
                    && matches!(
                        map.get("authorizationToken"),
                        Some(serde_json::Value::String(_))
                    )
                    && matches!(map.get("methodArn"), Some(serde_json::Value::String(_)))
            }
            _ => false,
        }
    }
}
impl ExtractRequest for apigw::ApiGatewayCustomAuthorizerRequest {
    const TYPE: RequestType = RequestType::APIGatewayLambdaAuthorizerToken;
    async fn extract(self) -> HttpData {
        HttpData::Request {
            raw_uri: None,
            method: None,
            route: None,
            client_ip: None,
            headers: self.authorization_token.map(|token| {
                HashMap::from([("Authorization".to_string(), vec![token.to_string()])])
            }),
            cookies: None,
            query: None,
            path_params: None,
            body: None,
        }
    }
}
impl IsValid for apigw::ApiGatewayCustomAuthorizerRequestTypeRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("type") {
            Some(serde_json::Value::String(t)) => {
                t == "REQUEST"
                    && matches!(map.get("methodArn"), Some(serde_json::Value::String(_)))
                    && matches!(map.get("headers"), Some(serde_json::Value::Object(_)))
                    && matches!(
                        map.get("queryStringParameters"),
                        Some(serde_json::Value::Object(_))
                    )
                    && match map.get("requestContext") {
                        Some(serde_json::Value::Object(request_context)) => {
                            matches!(
                                request_context.get("apiId"),
                                Some(serde_json::Value::String(_))
                            )
                        }
                        _ => false,
                    }
            }
            _ => false,
        }
    }
}
impl ExtractRequest for apigw::ApiGatewayCustomAuthorizerRequestTypeRequest {
    const TYPE: RequestType = RequestType::APIGatewayLambdaAuthorizerRequest;
    async fn extract(self) -> HttpData {
        let source_ip = self.request_context.identity.and_then(|i| i.source_ip);

        let (headers, cookies) = filter_headers(self.headers);

        HttpData::Request {
            raw_uri: self.path,
            method: self.http_method.map(|m| m.to_string()),
            route: self.resource,
            client_ip: source_ip,
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.multi_value_query_string_parameters),
            path_params: Some(self.path_parameters),
            body: None,
        }
    }
}
impl IsValid for alb::AlbTargetGroupRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("requestContext") {
            Some(serde_json::Value::Object(request_context)) => {
                matches!(
                    request_context.get("elb"),
                    Some(serde_json::Value::Object(_))
                )
            }
            _ => false,
        }
    }
}
impl ExtractRequest for alb::AlbTargetGroupRequest {
    const TYPE: RequestType = RequestType::Alb;
    async fn extract(self) -> HttpData {
        // Based on configuration, ALB provides headers EITHER in multi-value form OR in single-value form, never both.
        let (headers, cookies) = filter_headers(if self.multi_value_headers.is_empty() {
            self.headers
        } else {
            self.multi_value_headers
        });

        let query = if self.multi_value_query_string_parameters.is_empty() {
            query_to_optional_map(self.query_string_parameters)
        } else {
            query_to_optional_map(self.multi_value_query_string_parameters)
        };

        let content_type = headers
            .get("content-type")
            .and_then(|v| v.first())
            .map(String::as_str);
        let body = if let Some(body) = self.body {
            parse_body(body, self.is_base64_encoded, content_type)
                .await
                .ok()
                .flatten()
        } else {
            None
        };

        HttpData::Request {
            raw_uri: self.path,
            method: Some(self.http_method.to_string()),
            route: None,
            client_ip: None,
            headers: Some(headers),
            cookies,
            query,
            path_params: None,
            body,
        }
    }
}
impl IsValid for cloudwatch_events::CloudWatchEvent {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("source") {
            Some(serde_json::Value::String(source)) => source == "aws.events",
            _ => false,
        }
    }
}
impl IsValid for cloudwatch_logs::LogsEvent {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        matches!(map.get("awslogs"), Some(serde_json::Value::Object(_)))
    }
}
// TODO: CloudFrontRequestEvent
impl RecordSet for dynamodb::Event {
    const RECORD_KEY: &'static str = "dynamodb";
}
impl RecordSet for kinesis::KinesisEvent {
    const RECORD_KEY: &'static str = "kinesis";
}
impl RecordSet for s3::S3Event {
    const RECORD_KEY: &'static str = "s3";
}
impl RecordSet for sns::SnsEvent {
    const RECORD_KEY: &'static str = "sns";
}
impl IsValid for sqs::SqsEvent {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("Records") {
            Some(serde_json::Value::Array(records)) => records.iter().any(|record| match record {
                serde_json::Value::Object(record) => match record.get("eventSource") {
                    Some(serde_json::Value::String(source)) => source == "aws:sqs",
                    _ => false,
                },
                _ => false,
            }),
            _ => false,
        }
    }
}
// TODO:: SQSSNSEvent
// TODO:: AppSyncResolverEvent
impl IsValid for eventbridge::EventBridgeEvent {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        matches!(map.get("detail-type"), Some(serde_json::Value::String(_)))
            && match map.get("source") {
                Some(serde_json::Value::String(source)) => source != "aws.events",
                _ => false,
            }
    }
}
impl IsValid for lambda_function_urls::LambdaFunctionUrlRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("requestContext") {
            Some(serde_json::Value::Object(request_context)) => {
                match request_context.get("domainName") {
                    Some(serde_json::Value::String(domain_name)) => {
                        domain_name.contains(".lambda-url.")
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
}
impl ExtractRequest for lambda_function_urls::LambdaFunctionUrlRequest {
    const TYPE: RequestType = RequestType::LambdaFunctionUrl;
    async fn extract(self) -> HttpData {
        let (headers, cookies) = filter_headers(self.headers);

        let content_type = headers
            .get("content-type")
            .and_then(|v| v.first())
            .map(String::as_str);
        let body = if let Some(body) = self.body {
            parse_body(body, self.is_base64_encoded, content_type)
                .await
                .ok()
                .flatten()
        } else {
            None
        };

        HttpData::Request {
            raw_uri: self.raw_path,
            method: self.request_context.http.method,
            route: None,
            client_ip: self.request_context.http.source_ip,
            headers: Some(headers),
            cookies,
            query: to_optional_multimap(self.query_string_parameters),
            path_params: None,
            body,
        }
    }
}

#[allow(clippy::type_complexity)] // Come on!
fn filter_headers(
    headers: aws_lambda_events::http::HeaderMap,
) -> (
    HashMap<String, Vec<String>>,
    Option<HashMap<String, Vec<String>>>,
) {
    let mut filtered_headers = HashMap::with_capacity(headers.keys_len());
    let mut parsed_cookies = HashMap::new();

    for hdr in headers.keys() {
        let hdr = hdr.as_str();
        let val = headers.get_all(hdr).iter().filter_map(|v| v.to_str().ok());
        if hdr.eq_ignore_ascii_case("cookie") {
            for val in val {
                let cookies = axum_extra::extract::cookie::Cookie::split_parse_encoded(val);
                for cookie in cookies {
                    let Ok(cookie) = cookie else {
                        continue;
                    };
                    match parsed_cookies.entry(cookie.name().to_string()) {
                        Entry::Vacant(entry) => {
                            entry.insert(vec![cookie.value().to_string()]);
                        }
                        Entry::Occupied(entry) => {
                            entry.into_mut().push(cookie.value().to_string());
                        }
                    }
                }
            }
            continue;
        }
        filtered_headers.insert(hdr.to_lowercase(), val.map(str::to_string).collect());
    }

    (
        filtered_headers,
        if parsed_cookies.is_empty() {
            None
        } else {
            Some(parsed_cookies)
        },
    )
}

fn query_to_optional_map(
    query: aws_lambda_events::query_map::QueryMap,
) -> Option<HashMap<String, Vec<String>>> {
    if query.is_empty() {
        return None;
    }

    let iter = query.iter();
    let (lower, upper) = iter.size_hint();
    let mut query = HashMap::with_capacity(upper.unwrap_or(lower));
    for (k, v) in iter {
        match query.entry(k.to_string()) {
            Entry::Vacant(entry) => {
                entry.insert(vec![v.to_string()]);
            }
            Entry::Occupied(entry) => {
                entry.into_mut().push(v.to_string());
            }
        }
    }
    Some(query)
}

fn to_optional_multimap(map: HashMap<String, String>) -> Option<HashMap<String, Vec<String>>> {
    if map.is_empty() {
        return None;
    }
    let mut multimap = HashMap::with_capacity(map.len());

    for (k, v) in map {
        multimap.insert(k, vec![v]);
    }

    Some(multimap)
}
