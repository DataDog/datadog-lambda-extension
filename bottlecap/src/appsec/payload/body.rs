use base64::Engine;
use bytes::Buf;
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
        (mime::APPLICATION, sub) if sub == mime::JSON || sub == "vnd.api+json" => {
            Some(serde_json::from_slice(body)?)
        }
        (mime::APPLICATION, mime::WWW_FORM_URLENCODED) => Some(serde_html_form::from_bytes(body)?),
        (mime::APPLICATION | mime::TEXT, mime::XML) => {
            Some(serde_xml_rs::from_reader(body.reader())?)
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
