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
    pub(crate) fn reader<'a>(&'a self) -> Result<Option<Box<dyn Read + 'a>>, base64::DecodeError> {
        let Some(body) = &self.body else {
            return Ok(None);
        };

        if self.is_base64_encoded {
            let body = base64::engine::general_purpose::STANDARD.decode(body)?;
            let reader = Bytes::from(body).reader();
            Ok(Some(Box::new(reader)))
        } else {
            Ok(Some(Box::new(Cursor::new(body))))
        }
    }
}
