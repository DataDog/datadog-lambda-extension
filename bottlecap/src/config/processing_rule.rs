use serde::{Deserialize, Deserializer};
use serde_json::Value as JsonValue;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    ExcludeAtMatch,
    IncludeAtMatch,
    MaskSequences,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ProcessingRule {
    #[serde(rename = "type")]
    pub kind: Kind,
    pub name: String,
    pub pattern: String,
    pub replace_placeholder: Option<String>,
}

pub fn deserialize_processing_rules<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<ProcessingRule>>, D::Error>
where
    D: Deserializer<'de>,
{
    // Deserialize the JSON value using serde_json::Value
    let value: JsonValue = Deserialize::deserialize(deserializer)?;

    match value {
        JsonValue::String(s) => match serde_json::from_str(&s) {
            Ok(values) => Ok(Some(values)),
            Err(e) => {
                tracing::error!("Failed to parse processing rules: {}, ignoring", e);
                Ok(None)
            }
        },
        JsonValue::Array(a) => {
            let mut values = Vec::new();
            for v in a {
                match serde_json::from_value(v.clone()) {
                    Ok(rule) => values.push(rule),
                    Err(e) => {
                        tracing::error!("Failed to parse processing rule: {}, ignoring", e);
                    }
                }
            }
            if values.is_empty() {
                Ok(None)
            } else {
                Ok(Some(values))
            }
        }
        _ => Ok(None),
    }
}
