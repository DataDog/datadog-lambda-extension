use std::collections::HashMap;
use std::io::Cursor;

use serde::{Deserialize, Deserializer};

use crate::appsec::processor::InvocationPayload;
use crate::lifecycle::invocation::triggers::body::Body;

/// The expected payload of a response. This is different from trigger to trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExpectedResponseFormat {
    /// API Gateway style integration responses (REST and HTTP)
    ApiGatewayResponse,

    /// The entire paylaod is forwarded as-is, will try to parse as JSON.
    Raw,

    /// Unknown or unsupported response format
    #[default]
    Unknown,
}
impl ExpectedResponseFormat {
    pub(crate) const fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    pub(crate) fn parse(
        self,
        payload: &[u8],
    ) -> serde_json::Result<Option<Box<dyn InvocationPayload>>> {
        match self {
            Self::ApiGatewayResponse => {
                let payload = serde_json::from_slice::<ApiGatewayResponse>(payload)?;
                Ok(Some(Box::new(payload)))
            }
            Self::Raw => Ok(Some(Box::new(RawPayload {
                data: payload.to_vec(),
            }))),
            Self::Unknown => Ok(None),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
struct ApiGatewayResponse {
    status_code: i64,
    #[serde(deserialize_with = "nullable_lowercase_key", default)]
    headers: HashMap<String, String>,
    #[serde(deserialize_with = "nullable_lowercase_key", default)]
    multi_value_headers: HashMap<String, Vec<String>>,
    #[serde(flatten)]
    body: Body,
}
impl InvocationPayload for ApiGatewayResponse {
    #[cfg_attr(coverage_nightly, coverage(off))] // Only here to satisfy contract
    fn corresponding_response_format(&self) -> ExpectedResponseFormat {
        ExpectedResponseFormat::ApiGatewayResponse
    }

    fn response_status_code(&self) -> Option<i64> {
        Some(self.status_code)
    }
    fn response_headers_no_cookies(&self) -> HashMap<String, Vec<String>> {
        if self.multi_value_headers.is_empty() {
            self.headers
                .iter()
                .filter(|(k, _)| *k != "set-cookie")
                .map(|(k, v)| (k.clone(), vec![v.clone()]))
                .collect()
        } else {
            self.multi_value_headers.clone()
        }
    }
    fn response_body<'a>(&'a self) -> Option<Box<dyn std::io::Read + 'a>> {
        self.body.reader().ok().flatten()
    }
}

struct RawPayload {
    data: Vec<u8>,
}
impl InvocationPayload for RawPayload {
    #[cfg_attr(coverage_nightly, coverage(off))] // Only here to satisfy contract
    fn corresponding_response_format(&self) -> ExpectedResponseFormat {
        ExpectedResponseFormat::Raw
    }

    fn response_status_code(&self) -> Option<i64> {
        None
    }
    fn response_headers_no_cookies(&self) -> HashMap<String, Vec<String>> {
        HashMap::default()
    }
    fn response_body<'a>(&'a self) -> Option<Box<dyn std::io::Read + 'a>> {
        Some(Box::new(Cursor::new(&self.data)))
    }
}

fn nullable_lowercase_key<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    let Some(map) = Option::<HashMap<String, V>>::deserialize(deserializer)? else {
        return Ok(HashMap::default());
    };
    Ok(map
        .into_iter()
        .map(|(key, value)| (key.to_lowercase(), value))
        .collect())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_null_fields_in_apigw_response() {
        let response = r#"{
            "statusCode": 0,
            "headers": null,
            "multiValueHeaders": null,
            "body": null
        }"#;
        let response = ExpectedResponseFormat::ApiGatewayResponse
            .parse(response.as_bytes())
            .expect("response should have parsed cleanly")
            .expect("response should have been Some");
        assert!(response.response_body().is_none());
        assert!(response.response_headers_no_cookies().is_empty());
        assert_eq!(response.response_status_code(), Some(0));
    }
}
