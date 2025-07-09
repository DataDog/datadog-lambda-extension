use std::collections::HashMap;

use aws_lambda_events::{
    alb, apigw, cloudwatch_events, cloudwatch_logs, dynamodb, eventbridge, kinesis,
    lambda_function_urls, s3, sns, sqs,
};
use bytes::{Buf, Bytes};
use libddwaf::object::{WafArray, WafMap, WafObject};
use tracing::warn;

mod request;


trait IsValid {
    fn is_valid(map: &serde_json::Map<String, serde_json::Value>) -> bool;
}

struct HttpRequestData {
    source_ip: Option<String>,
    route: Option<String>,
    client_ip: Option<String>,
    raw_uri: Option<String>,
    headers: Option<HashMap<String, Vec<String>>>,
    cookies: Option<HashMap<String, Vec<String>>>,
    query: Option<HashMap<String, Vec<String>>>,
    path_params: Option<HashMap<String, String>>,
    body: Option<WafObject>,
    response_body: Option<WafObject>,
    response_status: Option<u32>,
}

trait ToWafMap {
    fn to_waf_map(self) -> WafMap;
}
impl ToWafMap for HttpRequestData {
    fn to_waf_map(self) -> WafMap {
        let count = [
            self.client_ip.is_some(),
            self.raw_uri.is_some(),
            self.headers.is_some(),
            self.cookies.is_some(),
            self.query.is_some(),
            self.path_params.is_some(),
            self.body.is_some(),
            self.response_body.is_some(),
            self.response_status.is_some(),
        ]
        .into_iter()
        .filter(|b| *b)
        .count();
        let mut map = WafMap::new(count as u64);
        let mut i = 0;

        if let Some(client_ip) = self.client_ip {
            map[i] = ("http.client_ip", client_ip.as_str()).into();
            i += 1;
        }
        if let Some(raw_uri) = self.raw_uri {
            map[i] = ("server.request.uri.raw", raw_uri.as_str()).into();
            i += 1;
        }
        if let Some(headers) = self.headers {
            map[i] = ("server.request.headers.no_cookies", headers.to_waf_map()).into();
            i += 1;
        }
        if let Some(cookies) = self.cookies {
            map[i] = ("server.request.cookies", cookies.to_waf_map()).into();
            i += 1;
        }
        if let Some(query) = self.query {
            map[i] = ("server.request.query", query.to_waf_map()).into();
            i += 1;
        }
        if let Some(path_params) = self.path_params {
            map[i] = ("server.request.path_params", path_params.to_waf_map()).into();
            i += 1;
        }
        if let Some(body) = self.body {
            map[i] = ("server.request.body", body).into();
            i += 1;
        }
        if let Some(response_body) = self.response_body {
            map[i] = ("server.response.body", response_body).into();
            i += 1;
        }
        if let Some(response_status) = self.response_status {
            map[i] = ("server.response.status", response_status).into();
            i += 1;
        }

        debug_assert_eq!(i, count); // Sanity check that we didn't over-allocate

        map
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

enum RequestType {
    APIGatewayV1, // Or Kong
    APIGatewayV2Http,
    APIGatewayV2Websocket,
    APIGatewayLambdaAuthorizerToken,
    APIGatewayLambdaAuthorizerRequest,
    Alb,
    LambdaFunctionUrl,
}


trait Extractor {
    const TYPE: RequestType;
    fn extract(self) -> HttpRequestData;
}

pub(super) fn extract_request_address_data(body: &Bytes) -> Option<WafMap> {
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
                return Some(val.extract().to_waf_map());
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

pub(super) fn extract_response_address_data(body: &Bytes) -> Option<WafMap> {
    let reader = body.clone().reader();
    let data: serde_json::Map<String, serde_json::Value> = match serde_json::from_reader(reader) {
        Ok(data) => data,
        Err(e) => {
            warn!("Failed to parse request body as JSON: {e}");
            return None;
        }
    };

    todo!("{data:?}")
}
