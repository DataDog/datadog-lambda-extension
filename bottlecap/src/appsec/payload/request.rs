use super::{Extractor, HTTPRequestData, IsValid};

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io::Read;

use aws_lambda_events::{
    alb, apigw, cloudwatch_events, cloudwatch_logs, dynamodb, eventbridge, kinesis,
    lambda_function_urls, s3, sns, sqs,
};
use bytes::Buf;
use libddwaf::object::{WAFObject, WAFString};
use tracing::debug;

/// Kong API Gateway events are a subset of [`apigw::ApiGatewayProxyRequest`].
#[derive(serde::Deserialize)]
pub(super) struct KongAPIGatewayEvent(apigw::ApiGatewayProxyRequest);

trait RecordSet {
    const RECORD_KEY: &'static str;
}
impl<T: RecordSet> IsValid for T {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("records") {
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
impl Extractor<'_> for apigw::ApiGatewayProxyRequest {
    fn extract(self) -> HTTPRequestData {
        let (headers, cookies) = filter_headers(self.multi_value_headers);

        // Headers are normalized to lowercase by [`filter_headers`].
        let content_type = headers["content-type"].first().map(String::as_str);
        let body = self.body.and_then(|b| {
            parse_body(b, self.is_base64_encoded, content_type)
                .ok()
                .flatten()
        });

        HTTPRequestData {
            source_ip: self.request_context.identity.source_ip.clone(),
            route: self.resource,
            client_ip: self.request_context.identity.source_ip, // API Gateway exposes the Client IP as the Source IP
            raw_uri: self.path,
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.query_string_parameters),
            path_params: Some(self.path_parameters),
            body,
            response_body: None,
            response_status: None,
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
impl Extractor<'_> for apigw::ApiGatewayV2httpRequest {
    fn extract(self) -> HTTPRequestData {
        let (headers, cookies) = filter_headers(self.headers);

        let content_type = headers["content-type"].first().map(String::as_str);
        let body = self.body.and_then(|b| {
            parse_body(b, self.is_base64_encoded, content_type)
                .ok()
                .flatten()
        });

        HTTPRequestData {
            source_ip: self.request_context.http.source_ip.clone(),
            route: self.route_key,
            client_ip: self.request_context.http.source_ip, // API Gateway exposes the Client IP as the Source IP
            raw_uri: self.raw_path,
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.query_string_parameters),
            path_params: Some(self.path_parameters),
            body,
            response_body: None,
            response_status: None,
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
impl Extractor<'_> for KongAPIGatewayEvent {
    fn extract(self) -> HTTPRequestData {
        self.0.extract()
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
impl Extractor<'_> for apigw::ApiGatewayWebsocketProxyRequest {
    fn extract(self) -> HTTPRequestData {
        let (headers, cookies) = filter_headers(self.multi_value_headers);

        let content_type = headers["content-type"].first().map(String::as_str);
        let body = self.body.and_then(|b| {
            parse_body(b, self.is_base64_encoded, content_type)
                .ok()
                .flatten()
        });

        HTTPRequestData {
            source_ip: self.request_context.identity.source_ip.clone(),
            route: self.resource,
            client_ip: self.request_context.identity.source_ip, // API Gateway exposes the Client IP as the Source IP
            raw_uri: self.path,
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.multi_value_query_string_parameters),
            path_params: Some(self.path_parameters),
            body,
            response_body: None,
            response_status: None,
        }
    }
}
impl IsValid for apigw::ApiGatewayCustomAuthorizerRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("type") {
            Some(serde_json::Value::String(t)) => {
                t == "token"
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
impl Extractor<'_> for apigw::ApiGatewayCustomAuthorizerRequest {
    fn extract(self) -> HTTPRequestData {
        HTTPRequestData {
            source_ip: None,
            route: None,
            client_ip: None,
            raw_uri: None,
            headers: self.authorization_token.map(|token| {
                HashMap::from([("Authorization".to_string(), vec![token.to_string()])])
            }),
            cookies: None,
            query: None,
            path_params: None,
            body: None,
            response_body: None,
            response_status: None,
        }
    }
}
impl IsValid for apigw::ApiGatewayCustomAuthorizerRequestTypeRequest {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool {
        match map.get("type") {
            Some(serde_json::Value::String(t)) => {
                t == "request"
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
impl Extractor<'_> for apigw::ApiGatewayCustomAuthorizerRequestTypeRequest {
    fn extract(self) -> HTTPRequestData {
        let source_ip = self.request_context.identity.and_then(|i| i.source_ip);

        let (headers, cookies) = filter_headers(self.headers);

        HTTPRequestData {
            source_ip: source_ip.clone(),
            route: self.resource,
            client_ip: source_ip,
            raw_uri: self.path,
            headers: Some(headers),
            cookies,
            query: query_to_optional_map(self.multi_value_query_string_parameters),
            path_params: Some(self.path_parameters),
            body: None,
            response_body: None,
            response_status: None,
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
impl Extractor<'_> for alb::AlbTargetGroupRequest {
    fn extract(self) -> HTTPRequestData {
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

        let content_type = headers["content-type"].first().map(String::as_str);
        let body = self.body.and_then(|b| {
            parse_body(b, self.is_base64_encoded, content_type)
                .ok()
                .flatten()
        });

        HTTPRequestData {
            source_ip: None,
            route: None,
            client_ip: None,
            raw_uri: self.path,
            headers: Some(headers),
            cookies,
            query,
            path_params: None,
            body,
            response_body: None,
            response_status: None,
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
        match map.get("records") {
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
impl Extractor<'_> for lambda_function_urls::LambdaFunctionUrlRequest {
    fn extract(self) -> HTTPRequestData {
        let (headers, cookies) = filter_headers(self.headers);

        let content_type = headers["content-type"].first().map(String::as_str);
        let body = self.body.and_then(|b| {
            parse_body(b, self.is_base64_encoded, content_type)
                .ok()
                .flatten()
        });

        HTTPRequestData {
            source_ip: self.request_context.http.source_ip.clone(),
            route: None,
            client_ip: self.request_context.http.source_ip,
            raw_uri: self.raw_path,
            headers: Some(headers),
            cookies,
            query: to_optional_multimap(self.query_string_parameters),
            path_params: None,
            body,
            response_body: None,
            response_status: None,
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
    let mut filtered_headers = HashMap::new();
    let mut parsed_cookies = HashMap::new();

    for (hdr, val) in headers {
        let Some(hdr) = hdr else { continue };
        let Ok(val) = val.to_str() else { continue };

        let hdr = hdr.as_str();
        if hdr.eq_ignore_ascii_case("cookie") {
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
            continue;
        }
        match filtered_headers.entry(hdr.to_lowercase()) {
            Entry::Vacant(entry) => {
                entry.insert(vec![val.to_string()]);
            }
            Entry::Occupied(entry) => {
                entry.into_mut().push(val.to_string());
            }
        }
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

fn parse_body(
    body: impl AsRef<[u8]>,
    is_base64_encoded: bool,
    content_type: Option<&str>,
) -> Result<Option<WAFObject>, Box<dyn std::error::Error>> {
    let body = body.as_ref();
    let reader: Box<dyn Read> = if is_base64_encoded {
        Box::new(base64::read::DecoderReader::new(
            body.reader(),
            &base64::engine::general_purpose::STANDARD,
        ))
    } else {
        Box::new(body.reader())
    };

    let mime_type = content_type
        .unwrap_or("application/json")
        .parse::<mime::Mime>()?;

    Ok(match (mime_type.type_(), mime_type.subtype()) {
        // text/json | application/json | application/vnd.api+json
        (mime::APPLICATION, sub) if sub == mime::JSON || sub == "vnd.api+json" => {
            Some(serde_json::from_reader(reader)?)
        }
        (mime::APPLICATION, mime::WWW_FORM_URLENCODED) => todo!(),
        (mime::APPLICATION | mime::TEXT, mime::XML) => todo!(),
        (mime::MULTIPART, mime::FORM_DATA) => todo!(),
        (mime::TEXT, mime::PLAIN) => Some(WAFString::new(body).into()),
        _ => {
            debug!("appsec: unsupported content type: {mime_type}");
            None
        }
    })
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
