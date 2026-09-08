use std::io::{Cursor, Read};

use base64::Engine;
use bytes::{Buf, Bytes};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub is_base64_encoded: bool,
}

impl Body {
    /// Obtains a reader to the data contained in this [`Body`], decoded from
    /// Base64 if [`Body::is_base64_encoded`] is `true`.
    ///
    /// Returns [`None`] if there is no body, including when it is an empty
    /// string, which is how many event sources represent the absence of a
    /// body (e.g. `GET` requests or `204` responses).
    pub(crate) fn reader<'a>(&'a self) -> Result<Option<Box<dyn Read + 'a>>, base64::DecodeError> {
        let Some(body) = &self.body else {
            return Ok(None);
        };

        if body.is_empty() {
            return Ok(None);
        }

        if self.is_base64_encoded {
            let body = base64::engine::general_purpose::STANDARD.decode(body)?;
            let reader = Bytes::from(body).reader();
            Ok(Some(Box::new(reader)))
        } else {
            Ok(Some(Box::new(Cursor::new(body))))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::read_to_string;

    use super::*;

    #[test]
    fn test_reader_no_body() {
        let body = Body {
            body: None,
            is_base64_encoded: false,
        };
        assert!(body.reader().expect("should not fail").is_none());
    }

    #[test]
    fn test_reader_empty_body() {
        let body = Body {
            body: Some(String::new()),
            is_base64_encoded: false,
        };
        assert!(body.reader().expect("should not fail").is_none());
    }

    #[test]
    fn test_reader_base64_body() {
        let body = Body {
            body: Some("eyJmb28iOiJiYXIifQ==".to_string()),
            is_base64_encoded: true,
        };
        let reader = body
            .reader()
            .expect("should not fail")
            .expect("should be Some");
        assert_eq!(
            read_to_string(reader).expect("should read cleanly"),
            r#"{"foo":"bar"}"#
        );
    }
}
