use std::collections::HashMap;

use aws_lambda_events::{
    alb, apigw, cloudwatch_events, cloudwatch_logs, dynamodb, eventbridge, kinesis,
    lambda_function_urls, s3, sns, sqs,
};
use bytes::{Buf, Bytes};
use libddwaf::object::{WafArray, WafMap, WafObject};
use tracing::warn;

mod body;
mod request;
mod response;

trait IsValid {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool;
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum HttpData {
    Request {
        raw_uri: Option<String>,
        method: Option<String>,
        route: Option<String>,
        client_ip: Option<String>,
        headers: Option<HashMap<String, Vec<String>>>,
        cookies: Option<HashMap<String, Vec<String>>>,
        query: Option<HashMap<String, Vec<String>>>,
        path_params: Option<HashMap<String, String>>,
        body: Option<WafObject>,
    },
    Response {
        status_code: Option<i64>,
        headers: Option<HashMap<String, Vec<String>>>,
        body: Option<WafObject>,
    },
}

pub(crate) trait ToWafMap {
    fn to_waf_map(self) -> WafMap;
}
impl ToWafMap for HttpData {
    fn to_waf_map(self) -> WafMap {
        match self {
            HttpData::Request {
                client_ip,
                raw_uri,
                headers: request_headers,
                cookies,
                query,
                path_params,
                body,
                ..
            } => {
                let count = [
                    client_ip.is_some(),
                    raw_uri.is_some(),
                    request_headers.is_some(),
                    cookies.is_some(),
                    query.is_some(),
                    path_params.is_some(),
                    body.is_some(),
                ]
                .into_iter()
                .filter(|b| *b)
                .count();
                let mut map = WafMap::new(count as u64);
                let mut i = 0;

                if let Some(client_ip) = client_ip {
                    map[i] = ("http.client_ip", client_ip.as_str()).into();
                    i += 1;
                }
                if let Some(raw_uri) = raw_uri {
                    map[i] = ("server.request.uri.raw", raw_uri.as_str()).into();
                    i += 1;
                }
                if let Some(headers) = request_headers {
                    map[i] = ("server.request.headers.no_cookies", headers.to_waf_map()).into();
                    i += 1;
                }
                if let Some(cookies) = cookies {
                    map[i] = ("server.request.cookies", cookies.to_waf_map()).into();
                    i += 1;
                }
                if let Some(query) = query {
                    map[i] = ("server.request.query", query.to_waf_map()).into();
                    i += 1;
                }
                if let Some(path_params) = path_params {
                    map[i] = ("server.request.path_params", path_params.to_waf_map()).into();
                    i += 1;
                }
                if let Some(body) = body {
                    map[i] = ("server.request.body", body).into();
                    i += 1;
                }
                debug_assert_eq!(i, count); // Sanity check that we didn't over-allocate

                map
            }
            HttpData::Response {
                status_code,
                headers,
                body,
                ..
            } => {
                let count = [status_code.is_some(), headers.is_some(), body.is_some()]
                    .into_iter()
                    .filter(|b| *b)
                    .count();
                let mut map = WafMap::new(count as u64);
                let mut i = 0;

                if let Some(response_status) = status_code {
                    map[i] = ("server.response.status", response_status).into();
                    i += 1;
                }
                if let Some(headers) = headers {
                    let mut headers = headers.clone();
                    headers.remove("set-cookie");
                    map[i] = ("server.response.headers.no_cookies", headers.to_waf_map()).into();
                    i += 1;
                }
                if let Some(response_body) = body {
                    map[i] = ("server.response.body", response_body).into();
                    i += 1;
                }

                debug_assert_eq!(i, count); // Sanity check that we didn't over-allocate

                map
            }
        }
    }
}
impl ToWafMap for HashMap<String, Vec<String>> {
    fn to_waf_map(self) -> WafMap {
        let mut map = WafMap::new(self.len() as u64);

        for (i, (k, v)) in self.into_iter().enumerate() {
            let mut arr = WafArray::new(v.len() as u64);
            for (j, v) in v.into_iter().enumerate() {
                arr[j] = v.as_str().into();
            }

            map[i] = (k.as_str(), arr).into();
        }

        map
    }
}
impl ToWafMap for HashMap<String, String> {
    fn to_waf_map(self) -> WafMap {
        let mut map = WafMap::new(self.len() as u64);

        for (i, (k, v)) in self.into_iter().enumerate() {
            map[i] = (k.as_str(), v.as_str()).into();
        }

        map
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestType {
    APIGatewayV1, // Or Kong
    APIGatewayV2Http,
    APIGatewayV2Websocket,
    APIGatewayLambdaAuthorizerToken,
    APIGatewayLambdaAuthorizerRequest,
    Alb,
    LambdaFunctionUrl,
}

trait ExtractRequest {
    const TYPE: RequestType;
    async fn extract(self) -> HttpData;
}
trait ExtractResponse {
    async fn extract(self) -> HttpData;
}

pub(super) async fn extract_request_address_data(body: &Bytes) -> Option<(HttpData, RequestType)> {
    let reader = body.clone().reader();
    let data: serde_json::Map<String, serde_json::Value> = match serde_json::from_reader(reader) {
        Ok(data) => data,
        Err(e) => {
            warn!("Failed to parse request body as JSON: {e}");
            return None;
        }
    };

    macro_rules! try_type {
        ($ty:ty, unsupported) => {
            if <$ty>::is_valid(&data) {
                return None;
            }
        };
        ($ty:ty) => {
            if <$ty>::is_valid(&data) {
                let Ok(val) = serde_json::from_value::<$ty>(serde_json::Value::Object(data)) else {
                    return None;
                };
                return Some((val.extract().await, <$ty>::TYPE));
            }
        };
    }

    // We try a bunch of types in a specific order to reduce the likelihood of incorrectly
    // identifying a payload as a different type. The "unsupported" variants are there to further
    // reduce the likelihood of us incorrectly identifying an unsupported payload as a supported
    // one.

    try_type!(apigw::ApiGatewayProxyRequest);
    try_type!(apigw::ApiGatewayV2httpRequest);
    try_type!(apigw::ApiGatewayWebsocketProxyRequest);
    try_type!(apigw::ApiGatewayCustomAuthorizerRequest);
    try_type!(apigw::ApiGatewayCustomAuthorizerRequestTypeRequest);
    try_type!(alb::AlbTargetGroupRequest);
    // CloudFrontEvent unsupported
    try_type!(cloudwatch_events::CloudWatchEvent, unsupported);
    try_type!(cloudwatch_logs::LogsEvent, unsupported);
    try_type!(dynamodb::Event, unsupported);
    try_type!(kinesis::KinesisEvent, unsupported);
    try_type!(s3::S3Event, unsupported);
    try_type!(sns::SnsEvent, unsupported);
    // SqsSnsEvent unsupported
    try_type!(sqs::SqsEvent, unsupported);
    // AppSyncResolverEvent unsupported // NB: This is GraphQL and maybe could be interesting
    try_type!(eventbridge::EventBridgeEvent, unsupported);
    try_type!(lambda_function_urls::LambdaFunctionUrlRequest);
    // StepFunctionEvent unsupported
    // LegacyStepFunctionEvent unsupported
    // NestedStepFunctionEvent unsupported
    // LegacyNestedStepFunctionEvent unsupported
    // LambdaRootStepFunctionPayload unsupported
    // LegacyLambdaRootStepFunctionPayload unsupported
    try_type!(request::KongAPIGatewayEvent); // IMPORTANT: Must ALWAYS be AFTER all the API Gateway payload types!

    // None of the payloads matched, so we don't have any address data to work with.
    None
}

pub(super) async fn extract_response_address_data(
    request_type: RequestType,
    body: &Bytes,
) -> Option<HttpData> {
    request_type.extract_response_address_data(body).await
}
impl RequestType {
    async fn extract_response_address_data(self, body: &Bytes) -> Option<HttpData> {
        macro_rules! match_types {
            ($($name:ident => $ty:ty),+) => {
                match self {$(
                    RequestType::$name => {
                        let body: $ty =
                            match serde_json::from_reader(body.clone().reader()) {
                                Ok(body) => body,
                                Err(e) => {
                                    warn!(concat!("appsec: failed to parse response payload from JSON as ", stringify!($ty),": {}"), e);
                                    return None;
                                }
                            };
                        body.extract().await
                    }
                ),+}
            }
        }

        Some(match_types! {
            APIGatewayV1 => apigw::ApiGatewayProxyResponse,
            APIGatewayV2Http => apigw::ApiGatewayV2httpResponse,
            APIGatewayV2Websocket => response::Opaque,
            APIGatewayLambdaAuthorizerToken => response::Opaque,
            APIGatewayLambdaAuthorizerRequest => response::Opaque,
            Alb => alb::AlbTargetGroupResponse,
            LambdaFunctionUrl => lambda_function_urls::LambdaFunctionUrlResponse
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use libddwaf::{waf_map, waf_object};

    #[tokio::test]
    async fn test_extract_api_gateway_v1_request() {
        let payload = r#"{
            "resource": "/{proxy+}",
            "path": "/path/to/resource",
            "httpMethod": "POST",
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json"
            },
            "multiValueHeaders": {
                "Accept": ["*/*"],
                "Content-Type": ["application/json"]
            },
            "queryStringParameters": {
                "foo": "bar"
            },
            "multiValueQueryStringParameters": {
                "foo": ["bar"]
            },
            "pathParameters": {
                "proxy": "/path/to/resource"
            },
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/{proxy+}",
                "httpMethod": "POST",
                "extendedRequestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "requestTime": "09/Apr/2015:12:34:56 +0000",
                "path": "/path/to/resource",
                "accountId": "123456789012",
                "protocol": "HTTP/1.1",
                "stage": "prod",
                "domainPrefix": "1234567890",
                "requestTimeEpoch": 1428582896000,
                "requestId": "c6af9ac6-7b61-11e6-9a41-93e8deadbeef",
                "identity": {
                    "cognitoIdentityPoolId": null,
                    "accountId": null,
                    "cognitoIdentityId": null,
                    "caller": null,
                    "accessKey": null,
                    "sourceIp": "127.0.0.1",
                    "cognitoAuthenticationType": null,
                    "cognitoAuthenticationProvider": null,
                    "userArn": null,
                    "userAgent": "Custom User Agent String",
                    "user": null
                },
                "domainName": "1234567890.execute-api.us-east-1.amazonaws.com",
                "apiId": "1234567890"
            },
            "body": "{\"test\":\"body\"}",
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        let (http_data, request_type) = result.expect("Expected result to be Some");
        assert_eq!(request_type, RequestType::APIGatewayV1);

        match http_data {
            HttpData::Request {
                method,
                route,
                client_ip,
                body,
                ..
            } => {
                assert_eq!(method, Some("POST".to_string()));
                assert_eq!(route, Some("/{proxy+}".to_string()));
                assert_eq!(client_ip, Some("127.0.0.1".to_string()));
                assert_eq!(body, Some(waf_map!(("test", "body")).into()));
            }
            HttpData::Response { .. } => panic!("Expected Request HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_v2_http_request() {
        let payload = r#"{
            "version": "2.0",
            "routeKey": "GET /httpapi/get",
            "rawPath": "/httpapi/get",
            "rawQueryString": "foo=bar",
            "cookies": ["cookie1", "cookie2"],
            "headers": {
                "Accept": "*/*",
                "Content-Type": "application/json",
                "Host": "example.amazonaws.com"
            },
            "queryStringParameters": {
                "foo": "bar"
            },
            "requestContext": {
                "accountId": "123456789012",
                "apiId": "1234567890",
                "authentication": {
                    "clientCert": {
                        "clientCertPem": "CERT_CONTENT",
                        "subjectDN": "www.example.com",
                        "issuerDN": "Example issuer",
                        "serialNumber": "a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1:a1",
                        "validity": {
                            "start": "May 28 12:30:02 2019 GMT",
                            "end": "Aug  5 09:36:04 2021 GMT"
                        }
                    }
                },
                "domainName": "example.amazonaws.com",
                "domainPrefix": "1234567890",
                "http": {
                    "method": "GET",
                    "path": "/httpapi/get",
                    "protocol": "HTTP/1.1",
                    "sourceIp": "192.168.1.1",
                    "userAgent": "agent"
                },
                "requestId": "JKJaXmPLvHcESHA=",
                "routeKey": "GET /httpapi/get",
                "stage": "$default",
                "time": "10/Mar/2020:05:28:40 +0000",
                "timeEpoch": 1583817320220
            },
            "body": "{\"message\":\"hello world\"}",
            "pathParameters": {
                "parameter1": "value1"
            },
            "isBase64Encoded": false,
            "stageVariables": {
                "stageVariable1": "value1",
                "stageVariable2": "value2"
            }
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        let (http_data, request_type) = result.expect("Expected result to be Some");
        assert_eq!(request_type, RequestType::APIGatewayV2Http);

        match http_data {
            HttpData::Request {
                method,
                route,
                client_ip,
                body,
                ..
            } => {
                assert_eq!(method, Some("GET".to_string()));
                assert_eq!(route, Some("GET /httpapi/get".to_string()));
                assert_eq!(client_ip, Some("192.168.1.1".to_string()));
                assert_eq!(body, Some(waf_map!(("message", "hello world")).into()));
            }
            HttpData::Response { .. } => panic!("Expected Request HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_websocket_request() {
        let payload = r#"{
            "requestContext": {
                "routeKey": "$connect",
                "messageId": null,
                "eventType": "CONNECT",
                "extendedRequestId": "JKJaXmPLvHcESHA=",
                "requestTime": "10/Mar/2020:05:28:40 +0000",
                "messageDirection": "IN",
                "stage": "prod",
                "connectedAt": 1583817320220,
                "requestTimeEpoch": 1583817320220,
                "identity": {
                    "cognitoIdentityPoolId": null,
                    "cognitoIdentityId": null,
                    "principalOrgId": null,
                    "cognitoAuthenticationType": null,
                    "userArn": null,
                    "userAgent": "Custom User Agent String",
                    "accountId": null,
                    "cognitoAuthenticationProvider": null,
                    "sourceIp": "127.0.0.1",
                    "accessKey": null,
                    "caller": null,
                    "principalUserId": null,
                    "user": null
                },
                "requestId": "JKJaXmPLvHcESHA=",
                "domainName": "example.amazonaws.com",
                "connectionId": "JKJaXmPLvHcESHA=",
                "apiId": "1234567890"
            },
            "headers": {
                "Host": "example.amazonaws.com",
                "User-Agent": "Custom User Agent String",
                "Content-Type": "application/json"
            },
            "multiValueHeaders": {
                "Host": ["example.amazonaws.com"],
                "User-Agent": ["Custom User Agent String"],
                "Content-Type": ["application/json"]
            },
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        let (http_data, request_type) = result.expect("Expected result to be Some");
        assert_eq!(request_type, RequestType::APIGatewayV2Websocket);

        match http_data {
            HttpData::Request { client_ip, .. } => {
                assert_eq!(client_ip, Some("127.0.0.1".to_string()));
            }
            HttpData::Response { .. } => panic!("Expected Request HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_alb_request() {
        let payload = r#"{
            "requestContext": {
                "elb": {
                    "targetGroupArn": "arn:aws:elasticloadbalancing:us-east-1:123456789012:targetgroup/lambda-279XGJDqGZ5rsrHC2Fjr/49e9d65c45c6791a"
                }
            },
            "httpMethod": "GET",
            "path": "/lambda",
            "queryStringParameters": {
                "query": "1234ABCD"
            },
            "headers": {
                "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/apng,*/*;q=0.8",
                "Accept-Encoding": "gzip, deflate, br",
                "Accept-Language": "en-US,en;q=0.9",
                "Connection": "keep-alive",
                "Host": "lambda-alb-123578498.us-east-1.elb.amazonaws.com",
                "Upgrade-Insecure-Requests": "1",
                "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/71.0.3578.98 Safari/537.36",
                "X-Amzn-Trace-Id": "Root=1-5c536348-3d683b8b04734faae651f476",
                "X-Forwarded-For": "72.12.164.125",
                "X-Forwarded-Port": "80",
                "X-Forwarded-Proto": "http",
                "Content-Type": "text/plain"
            },
            "multiValueHeaders": {
                "Accept": ["text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/apng,*/*;q=0.8"],
                "Accept-Encoding": ["gzip, deflate, br"],
                "Accept-Language": ["en-US,en;q=0.9"],
                "Connection": ["keep-alive"],
                "Host": ["lambda-alb-123578498.us-east-1.elb.amazonaws.com"],
                "Upgrade-Insecure-Requests": ["1"],
                "User-Agent": ["Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/71.0.3578.98 Safari/537.36"],
                "X-Amzn-Trace-Id": ["Root=1-5c536348-3d683b8b04734faae651f476"],
                "X-Forwarded-For": ["72.12.164.125"],
                "X-Forwarded-Port": ["80"],
                "X-Forwarded-Proto": ["http"],
                "Content-Type": ["text/plain"]
            },
            "body": "",
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        let (http_data, request_type) = result.expect("Expected result to be Some");
        assert_eq!(request_type, RequestType::Alb);

        match http_data {
            HttpData::Request {
                method,
                client_ip,
                body,
                ..
            } => {
                assert_eq!(method, Some("GET".to_string()));
                // ALB implementation doesn't extract client IP from X-Forwarded-For header
                assert_eq!(client_ip, None);
                assert_eq!(body, Some(waf_object!("")));
            }
            HttpData::Response { .. } => panic!("Expected Request HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_lambda_function_url_request() {
        let payload = r#"{
            "version": "2.0",
            "routeKey": "$default",
            "rawPath": "/my/path",
            "rawQueryString": "parameter1=value1&parameter1=value2&parameter2=value",
            "cookies": [
                "cookie1",
                "cookie2"
            ],
            "headers": {
                "Header1": "value1",
                "Header2": "value2",
                "Content-Type": "text/plain"
            },
            "queryStringParameters": {
                "parameter1": "value1,value2",
                "parameter2": "value"
            },
            "requestContext": {
                "accountId": "123456789012",
                "apiId": "r3pmxmplak",
                "domainName": "r3pmxmplak.lambda-url.us-east-2.on.aws",
                "domainPrefix": "r3pmxmplak",
                "http": {
                    "method": "POST",
                    "path": "/my/path",
                    "protocol": "HTTP/1.1",
                    "sourceIp": "123.123.123.123",
                    "userAgent": "agent"
                },
                "requestId": "id",
                "routeKey": "$default",
                "stage": "$default",
                "time": "12/Mar/2020:19:03:58 +0000",
                "timeEpoch": 1584043438390
            },
            "body": "Hello from Lambda!",
            "pathParameters": null,
            "isBase64Encoded": false,
            "stageVariables": null
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        let (http_data, request_type) = result.expect("Expected result to be Some");
        assert_eq!(request_type, RequestType::LambdaFunctionUrl);

        match http_data {
            HttpData::Request {
                method,
                client_ip,
                body,
                ..
            } => {
                assert_eq!(method, Some("POST".to_string()));
                assert_eq!(client_ip, Some("123.123.123.123".to_string()));
                assert_eq!(body, Some(waf_object!("Hello from Lambda!")));
            }
            HttpData::Response { .. } => panic!("Expected Request HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_unsupported_sns_event() {
        let payload = r#"{
            "Records": [
                {
                    "EventSource": "aws:sns",
                    "EventVersion": "1.0",
                    "EventSubscriptionArn": "arn:aws:sns:us-east-1:123456789012:example-topic:2bcfbf39-05c3-41de-beaa-fcfcc21c8f55",
                    "Sns": {
                        "Type": "Notification",
                        "MessageId": "95df01b4-ee98-5cb9-9903-4c221d41eb5e",
                        "TopicArn": "arn:aws:sns:us-east-1:123456789012:example-topic",
                        "Subject": "example subject",
                        "Message": "example message",
                        "Timestamp": "1970-01-01T00:00:00.000Z",
                        "SignatureVersion": "1",
                        "Signature": "EXAMPLE",
                        "SigningCertUrl": "EXAMPLE",
                        "UnsubscribeUrl": "EXAMPLE"
                    }
                }
            ]
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        // SNS events are explicitly unsupported
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_unsupported_sqs_event() {
        let payload = r#"{
            "Records": [
                {
                    "messageId": "19dd0b57-b21e-4ac1-bd88-01bbb068cb78",
                    "receiptHandle": "MessageReceiptHandle",
                    "body": "Hello from SQS!",
                    "attributes": {
                        "ApproximateReceiveCount": "1",
                        "SentTimestamp": "1523232000000",
                        "SenderId": "123456789012",
                        "ApproximateFirstReceiveTimestamp": "1523232000001"
                    },
                    "messageAttributes": {},
                    "md5OfBody": "7b270e59b47ff90a553787216d55d91d",
                    "eventSource": "aws:sqs",
                    "eventSourceARN": "arn:aws:sqs:us-east-1:123456789012:MyQueue",
                    "awsRegion": "us-east-1"
                }
            ]
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        // SQS events are explicitly unsupported
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_invalid_json() {
        let payload = r#"{"invalid": json}"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_unrecognized_event_structure() {
        let payload = r#"{
            "some": "unrecognized",
            "event": "structure",
            "that": "doesnt",
            "match": "any known patterns"
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_empty_payload() {
        let payload = "{}";

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_malformed_api_gateway_event() {
        let payload = r#"{
            "resource": "/{proxy+}",
            "path": "/path/to/resource",
            "httpMethod": "POST",
            "headers": {
                "Content-Type": "application/json"
            },
            "requestContext": {
                "stage": "prod"
            }
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        // This malformed event should not be recognized since it's missing required fields
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_with_base64_encoded_body() {
        let payload = r#"{
            "resource": "/{proxy+}",
            "path": "/path/to/resource",
            "httpMethod": "POST",
            "headers": {
                "Content-Type": "application/json"
            },
            "multiValueHeaders": {
                "Content-Type": ["application/json"]
            },
            "requestContext": {
                "resourceId": "123456",
                "resourcePath": "/{proxy+}",
                "httpMethod": "POST",
                "stage": "prod",
                "identity": {
                    "sourceIp": "127.0.0.1"
                }
            },
            "body": "eyJ0ZXN0IjoiYm9keSJ9",
            "isBase64Encoded": true
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_request_address_data(&bytes).await;

        let (http_data, request_type) = result.expect("Expected result to be Some");
        assert_eq!(request_type, RequestType::APIGatewayV1);

        match http_data {
            HttpData::Request { body, .. } => {
                assert_eq!(body, Some(waf_map!(("test", "body")).into()));
            }
            HttpData::Response { .. } => panic!("Expected Request HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_v2_http_response() {
        let payload = r#"{
            "statusCode": 200,
            "multiValueHeaders": {
                "Content-Type": ["application/json"],
                "X-Custom-Header": ["custom-value"]
            },
            "body": "{\"message\": \"success\", \"data\": \"test\"}",
            "isBase64Encoded": false,
            "cookies": []
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::APIGatewayV2Http, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                assert_eq!(status_code, Some(200));
                assert_eq!(
                    headers,
                    Some(HashMap::from([
                        (
                            "content-type".to_string(),
                            vec!["application/json".to_string()]
                        ),
                        (
                            "x-custom-header".to_string(),
                            vec!["custom-value".to_string()]
                        ),
                    ]))
                );
                assert_eq!(
                    body,
                    Some(waf_map!(("message", "success"), ("data", "test")).into())
                );
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_v2_http_response_no_body() {
        let payload = r#"{
            "statusCode": 204,
            "headers": { "Content-Length": "0" },
            "cookies": []
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::APIGatewayV2Http, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                assert_eq!(status_code, Some(204));
                assert_eq!(
                    headers,
                    Some(HashMap::from([(
                        "content-length".to_string(),
                        vec!["0".to_string()]
                    )]))
                );
                assert_eq!(body, None);
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_v2_http_response_base64_body() {
        let payload = r#"{
            "statusCode": 200,
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "eyJtZXNzYWdlIjogImVuY29kZWQifQ==",
            "isBase64Encoded": true,
            "cookies": []
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::APIGatewayV2Http, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response { body, .. } => {
                assert_eq!(body, Some(waf_map!(("message", "encoded")).into()));
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_websocket_response() {
        let payload = r#"{
            "statusCode": 200,
            "body": "Message sent"
        }"#;

        let bytes = Bytes::from(payload);
        let result =
            extract_response_address_data(RequestType::APIGatewayV2Websocket, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                // Websocket responses use Opaque type, which returns None for all fields
                assert_eq!(status_code, None);
                assert_eq!(headers, None);
                assert_eq!(body, None);
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_lambda_authorizer_token_response() {
        let payload = r#"{
            "principalId": "user123",
            "policyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": "execute-api:Invoke",
                        "Resource": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/users"
                    }
                ]
            }
        }"#;

        let bytes = Bytes::from(payload);
        let result =
            extract_response_address_data(RequestType::APIGatewayLambdaAuthorizerToken, &bytes)
                .await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                // Lambda authorizer responses use Opaque type, which returns None for all fields
                assert_eq!(status_code, None);
                assert_eq!(headers, None);
                assert_eq!(body, None);
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_api_gateway_lambda_authorizer_request_response() {
        let payload = r#"{
            "principalId": "user123",
            "policyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": "execute-api:Invoke",
                        "Resource": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/users"
                    }
                ]
            },
            "context": {
                "userId": "user123"
            }
        }"#;

        let bytes = Bytes::from(payload);
        let result =
            extract_response_address_data(RequestType::APIGatewayLambdaAuthorizerRequest, &bytes)
                .await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                // Lambda authorizer responses use Opaque type, which returns None for all fields
                assert_eq!(status_code, None);
                assert_eq!(headers, None);
                assert_eq!(body, None);
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_alb_target_group_response() {
        let payload = r#"{
            "statusCode": 200,
            "statusDescription": "200 OK",
            "headers": {
                "Content-Type": "text/html",
                "Set-Cookie": "cookie1=value1; Path=/",
                "X-Custom-Header": "custom-value"
            },
            "body": "<html><body><h1>Hello from ALB!</h1></body></html>",
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::Alb, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                assert_eq!(status_code, Some(200));
                assert_eq!(
                    headers,
                    Some(HashMap::from([
                        ("content-type".to_string(), vec!["text/html".to_string()]),
                        (
                            "x-custom-header".to_string(),
                            vec!["custom-value".to_string()]
                        ),
                    ]))
                );
                assert!(body.is_none()); // text/html is not relevant for security
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_alb_target_group_response_json_body() {
        let payload = r#"{
            "statusCode": 201,
            "statusDescription": "201 Created",
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "{\"id\": 123, \"name\": \"test\", \"active\": true}",
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::Alb, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code, body, ..
            } => {
                assert_eq!(status_code, Some(201));
                assert_eq!(
                    body,
                    Some(waf_map!(("id", 123u64), ("name", "test"), ("active", true)).into())
                );
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_alb_target_group_response_empty_body() {
        let payload = r#"{
            "statusCode": 204,
            "statusDescription": "204 No Content",
            "headers": {
                "Content-Type": "text/plain"
            },
            "isBase64Encoded": false
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::Alb, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code, body, ..
            } => {
                assert_eq!(status_code, Some(204));
                assert_eq!(body, None);
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_lambda_function_url_response() {
        let payload = r#"{
            "statusCode": 200,
            "headers": {
                "Content-Type": "application/json",
                "X-Custom-Header": "custom-value",
                "Cache-Control": "no-cache"
            },
            "body": "{\"message\": \"Hello from Lambda function URL!\", \"timestamp\": 1234567890}",
            "isBase64Encoded": false,
            "cookies": []
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::LambdaFunctionUrl, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                assert_eq!(status_code, Some(200));
                assert_eq!(
                    headers,
                    Some(HashMap::from([
                        (
                            "content-type".to_string(),
                            vec!["application/json".to_string()]
                        ),
                        (
                            "x-custom-header".to_string(),
                            vec!["custom-value".to_string()]
                        ),
                        ("cache-control".to_string(), vec!["no-cache".to_string()]),
                    ]))
                );
                assert_eq!(
                    body,
                    Some(
                        waf_map!(
                            ("message", "Hello from Lambda function URL!"),
                            ("timestamp", 1_234_567_890u64)
                        )
                        .into()
                    )
                );
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_lambda_function_url_response_base64_body() {
        let payload = r#"{
            "statusCode": 200,
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "eyJzdGF0dXMiOiAib2sifQ==",
            "isBase64Encoded": true,
            "cookies": []
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::LambdaFunctionUrl, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response { body, .. } => {
                assert_eq!(body, Some(waf_map!(("status", "ok")).into()));
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_lambda_function_url_response_error_status() {
        let payload = r#"{
            "statusCode": 500,
            "headers": {
                "Content-Type": "application/json"
            },
            "body": "{\"error\": \"Internal Server Error\", \"code\": 500}",
            "isBase64Encoded": false,
            "cookies": []
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::LambdaFunctionUrl, &bytes).await;

        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code, body, ..
            } => {
                assert_eq!(status_code, Some(500));
                assert_eq!(
                    body,
                    Some(waf_map!(("error", "Internal Server Error"), ("code", 500u64)).into())
                );
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }

    #[tokio::test]
    async fn test_extract_response_invalid_json() {
        let payload = r#"{"invalid": json}"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::APIGatewayV2Http, &bytes).await;

        // Should return None for invalid JSON
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_response_malformed_structure() {
        let payload = r#"{
            "some": "unrecognized",
            "structure": "that doesnt match expected response format"
        }"#;

        let bytes = Bytes::from(payload);
        let result = extract_response_address_data(RequestType::APIGatewayV2Http, &bytes).await;

        // Should return None for malformed structure
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_extract_opaque_response_shouldnt_cause_failures() {
        // Test that opaque payloads (like custom authorizer responses) don't cause failures
        let complex_payload = r#"{
            "principalId": "user123",
            "policyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": "execute-api:Invoke",
                        "Resource": "arn:aws:execute-api:us-east-1:123456789012:abcdef123/test/GET/users"
                    }
                ]
            },
            "context": {
                "userId": "user123",
                "department": "engineering",
                "permissions": ["read", "write"]
            },
            "usageIdentifierKey": "some-key"
        }"#;

        let bytes = Bytes::from(complex_payload);
        let result =
            extract_response_address_data(RequestType::APIGatewayLambdaAuthorizerToken, &bytes)
                .await;

        // Should handle complex opaque payloads gracefully
        let http_data = result.expect("Expected result to be Some");
        match http_data {
            HttpData::Response {
                status_code,
                headers,
                body,
            } => {
                // Opaque responses should return None for all fields
                assert_eq!(status_code, None);
                assert_eq!(headers, None);
                assert_eq!(body, None);
            }
            HttpData::Request { .. } => panic!("Expected Response HttpData"),
        }
    }
}
