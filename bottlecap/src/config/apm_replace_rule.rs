use serde::{Deserialize, Deserializer};
use datadog_trace_obfuscation::replacer::{ReplaceRule, parse_rules_from_string};


pub fn deserialize_apm_replace_rules<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<ReplaceRule>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: String = String::deserialize(deserializer)?;
    let rules: Vec<ReplaceRule> = parse_rules_from_string(&value).map_err(|e| {
        serde::de::Error::custom(format!("Failed to deserialize replace rules: {e}"))
    })?;
    Ok(Some(rules))
}
