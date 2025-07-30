use std::collections::HashMap;
use std::io::Cursor;

use serde::Deserialize;

use crate::appsec::processor::InvocationPayload;
use crate::lifecycle::invocation::triggers::{body::Body, lowercase_key};

/// The expected payload of a response. This is different from trigger to trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedResponseFormat {
    /// API Gateway style integration responses (REST and HTTP)
    ApiGatewayResponse,

    /// The entire paylaod is forwarded as-is, will try to parse as JSON.
    Raw,

    /// Unknown or unsupported response format
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
impl Default for ExpectedResponseFormat {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
struct ApiGatewayResponse {
    status_code: i64,
    #[serde(deserialize_with = "lowercase_key")]
    headers: HashMap<String, String>,
    #[serde(deserialize_with = "lowercase_key")]
    multi_value_headers: HashMap<String, Vec<String>>,
    #[serde(flatten)]
    body: Body,
}
impl InvocationPayload for ApiGatewayResponse {
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
