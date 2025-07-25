use base64::Engine;
use libddwaf::object::{WafMap, WafObject, WafString};
use mime::Mime;
use tracing::{debug, warn};

pub(super) async fn parse_body(
    body: impl AsRef<[u8]>,
    is_base64_encoded: bool,
    content_type: Option<&str>,
) -> Result<Option<WafObject>, Box<dyn std::error::Error>> {
    if is_base64_encoded {
        let body = base64::engine::general_purpose::STANDARD.decode(body)?;
        return Box::pin(parse_body(body, false, content_type)).await;
    }

    let body = body.as_ref();
    let mime_type = match content_type
        .unwrap_or("application/json")
        .parse::<mime::Mime>()
    {
        Ok(mime) => mime,
        Err(e) => return Err(e.into()),
    };

    Ok(match (mime_type.type_(), mime_type.subtype()) {
        // text/json | application/json | application/vnd.api+json
        (mime::APPLICATION, sub)
            if sub == mime::JSON
                || (sub == "vnd.api" && mime_type.suffix() == Some(mime::JSON)) =>
        {
            Some(serde_json::from_slice(body)?)
        }
        (mime::APPLICATION, mime::WWW_FORM_URLENCODED) => {
            let pairs: Vec<(String, String)> = serde_html_form::from_bytes(body)?;
            let mut res = WafMap::new(pairs.len() as u64);
            for (i, (key, value)) in pairs.into_iter().enumerate() {
                res[i] = (key.as_str(), value.as_str()).into();
            }
            Some(res.into())
        }
        (mime::APPLICATION | mime::TEXT, mime::XML) => {
            // XML parsing is not currently supported due to serde_xml_rs limitations
            debug!(
                "appsec: XML parsing not supported, returning None for content type: {mime_type}"
            );
            None
        }
        (mime::MULTIPART, mime::FORM_DATA) => {
            let Some(boundary) = mime_type.get_param("boundary") else {
                warn!("appsec: cannot attempt parsing multipart/form-data without boundary");
                return Ok(None);
            };
            // We have to go through this dance because [`multer::Multipart`] requires an async stream.
            let body = body.to_vec();
            let reader = futures::stream::iter([Result::<Vec<u8>, std::io::Error>::Ok(body)]);
            let mut multipart = multer::Multipart::new(reader, boundary.as_str());

            let mut fields = Vec::new();
            while let Some(field) = multipart.next_field().await? {
                let Some(name) = field.name().map(str::to_string) else {
                    continue;
                };
                let Some(content_type) = field.content_type().map(Mime::to_string) else {
                    continue;
                };
                let Some(value) = Box::pin(parse_body(
                    field.bytes().await?,
                    false,
                    Some(content_type.as_ref()),
                ))
                .await?
                else {
                    continue;
                };
                fields.push((name, value));
            }
            let mut res = WafMap::new(fields.len() as u64);
            for (i, (name, value)) in fields.into_iter().enumerate() {
                res[i] = (name.as_str(), value).into();
            }
            Some(res.into())
        }
        (mime::TEXT, mime::PLAIN) => Some(WafString::new(body).into()),
        _ => {
            debug!("appsec: unsupported content type: {mime_type}");
            None
        }
    })
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
mod tests {
    use libddwaf::{waf_array, waf_map, waf_object};

    use super::*;

    #[tokio::test]
    async fn test_parse_json_body_with_default_content_type() {
        let json_body = r#"{"test": "value"}"#;

        let result = parse_body(json_body, false, None)
            .await
            .expect("should parse application/json")
            .expect("should produce some value");
        assert_eq!(result, waf_map!(("test", "value")));
    }

    #[tokio::test]
    async fn test_parse_json_body() {
        let json_body = r#"{"test": "value"}"#;

        for content_type in [
            "application/json",
            "application/vnd.api+json",
            "application/json; charset=utf-8",
        ] {
            let result = parse_body(json_body, false, Some(content_type))
                .await
                .expect("should parse successfully")
                .expect("should produce some value");
            assert_eq!(result, waf_map!(("test", "value")));
        }
    }

    #[tokio::test]
    async fn test_parse_invalid_json_body() {
        let invalid_json = r#"{"key": "value", "invalid": }"#;
        let result = parse_body(invalid_json, false, Some("application/json")).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parse_xml_body() {
        let xml_body = r#"<?xml version="1.0"?><root><item>value</item></root>"#;

        // Test application/xml - currently not supported, should return None
        let result = parse_body(xml_body, false, Some("application/xml")).await;
        assert!(result.is_ok());
        let parsed = result.expect("should handle XML content type");
        assert!(parsed.is_none(), "XML parsing is not currently supported");

        // Test text/xml - currently not supported, should return None
        let result = parse_body(xml_body, false, Some("text/xml")).await;
        assert!(result.is_ok());
        let parsed = result.expect("should handle XML content type");
        assert!(parsed.is_none(), "XML parsing is not currently supported");
    }

    #[tokio::test]
    async fn test_parse_invalid_xml_body() {
        let invalid_xml = "<root><unclosed>value</root>";
        let result = parse_body(invalid_xml, false, Some("application/xml")).await;

        // XML parsing is not currently supported, so it should return Ok(None) regardless of validity
        assert!(result.is_ok());
        let parsed = result.expect("should handle XML content type");
        assert!(parsed.is_none(), "XML parsing is not currently supported");
    }

    #[tokio::test]
    async fn test_parse_url_encoded_body() {
        let form_data = "key1=value1&key2=value2&key3=value%20with%20spaces";
        let result = parse_body(form_data, false, Some("application/x-www-form-urlencoded"))
            .await
            .expect("should parse URL-encoded data")
            .expect("should produce some value");
        assert_eq!(
            result,
            waf_map!(
                ("key1", "value1"),
                ("key2", "value2"),
                ("key3", "value with spaces")
            )
        );
    }

    #[tokio::test]
    async fn test_parse_invalid_url_encoded_body() {
        let invalid_form_data = "key1=value1&key2=&=value3";
        let result = parse_body(
            invalid_form_data,
            false,
            Some("application/x-www-form-urlencoded"),
        )
        .await;

        // URL-encoded parsing should be permissive and not fail on malformed data
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_parse_multipart_form_data() {
        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let multipart_body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"field1\"\r\nContent-Type: text/plain\r\n\r\nvalue1\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"field2\"\r\nContent-Type: application/json\r\n\r\n{{\"key\": \"value\"}}\r\n--{boundary}--\r\n"
        );

        let content_type = format!("multipart/form-data; boundary={boundary}");
        let result = parse_body(multipart_body, false, Some(&content_type))
            .await
            .expect("should parse multipart form data")
            .expect("should produce some value");
        assert_eq!(
            result,
            waf_map!(("field1", "value1"), ("field2", waf_map!(("key", "value"))))
        );
    }

    #[tokio::test]
    async fn test_parse_multipart_form_data_without_boundary() {
        let multipart_body = "some content";
        let result = parse_body(multipart_body, false, Some("multipart/form-data")).await;

        assert!(result.is_ok());
        let parsed = result.expect("should handle multipart without boundary");
        assert!(parsed.is_none()); // Should return None due to missing boundary
    }

    #[tokio::test]
    async fn test_parse_text_plain_body() {
        let text_body = "This is plain text content";
        let result = parse_body(text_body, false, Some("text/plain"))
            .await
            .expect("should parse plain text")
            .expect("should produce some value");
        assert_eq!(result, waf_object!("This is plain text content"));
    }

    #[tokio::test]
    async fn test_parse_base64_encoded_body() {
        let json_body = r#"{"key": "value"}"#;
        let base64_body = base64::engine::general_purpose::STANDARD.encode(json_body);

        let result = parse_body(base64_body, true, Some("application/json"))
            .await
            .expect("should parse base64 encoded JSON")
            .expect("should produce some value");
        assert_eq!(result, waf_map!(("key", "value")));
    }

    #[tokio::test]
    async fn test_parse_invalid_base64_encoded_body() {
        let invalid_base64 = "invalid_base64!@#$%";
        let result = parse_body(invalid_base64, true, Some("application/json")).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parse_unsupported_content_type() {
        let body = "some content";
        let result = parse_body(body, false, Some("application/octet-stream")).await;

        assert!(result.is_ok());
        let parsed = result.expect("should handle unsupported content type");
        assert!(parsed.is_none()); // Should return None for unsupported content types
    }

    #[tokio::test]
    async fn test_parse_invalid_content_type() {
        let body = "some content";
        let result = parse_body(body, false, Some("invalid/content-type")).await;

        assert!(result.is_ok());
        let parsed = result.expect("should handle invalid content type");
        assert!(parsed.is_none()); // Should return None for invalid content type
    }

    #[tokio::test]
    async fn test_parse_empty_body() {
        let result = parse_body("", false, Some("application/json")).await;

        assert!(result.is_err()); // Empty JSON should fail to parse
    }

    #[tokio::test]
    async fn test_parse_malformed_mime_type() {
        let body = "some content";
        let result = parse_body(body, false, Some("not-a-valid-mime-type")).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parse_nested_json_body() {
        let nested_json = r#"{"user": {"name": "John", "age": 30}, "tags": ["tag1", "tag2"]}"#;
        let result = parse_body(nested_json, false, Some("application/json"))
            .await
            .expect("should parse nested JSON")
            .expect("should produce some value");
        assert_eq!(
            result,
            waf_map!(
                ("user", waf_map!(("name", "John"), ("age", 30u64))),
                ("tags", waf_array!("tag1", "tag2"))
            )
        );
    }

    #[tokio::test]
    async fn test_parse_large_json_body() {
        let large_json = format!(r#"{{"data": "{}"}}"#, "x".repeat(10000));
        let result = parse_body(large_json, false, Some("application/json"))
            .await
            .expect("should parse large JSON")
            .expect("should produce some value");
        assert_eq!(result, waf_map!(("data", "x".repeat(10000).as_str())));
    }

    #[tokio::test]
    async fn test_parse_multipart_with_missing_field_name() {
        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let multipart_body = format!(
            "--{boundary}\r\nContent-Disposition: form-data\r\nContent-Type: text/plain\r\n\r\nvalue1\r\n--{boundary}--\r\n"
        );

        let content_type = format!("multipart/form-data; boundary={boundary}");
        let result = parse_body(multipart_body, false, Some(&content_type)).await;

        assert!(result.is_ok());
        let parsed = result.expect("should handle multipart with missing field name");
        assert!(parsed.is_some());
    }

    #[tokio::test]
    async fn test_parse_multipart_with_missing_content_type() {
        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let multipart_body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"field1\"\r\n\r\nvalue1\r\n--{boundary}--\r\n"
        );

        let content_type = format!("multipart/form-data; boundary={boundary}");
        let result = parse_body(multipart_body, false, Some(&content_type)).await;

        assert!(result.is_ok());
        let parsed = result.expect("should handle multipart with missing content type");
        assert!(parsed.is_some());
    }
}
