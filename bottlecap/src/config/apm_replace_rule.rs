use datadog_trace_obfuscation::replacer::{parse_rules_from_string, ReplaceRule};
use serde::de::{Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json;
use std::fmt;

#[derive(Deserialize, Serialize)]
struct ReplaceRuleYaml {
    name: String,
    pattern: String,
    repl: String,
}

struct StringOrReplaceRulesVisitor;

impl<'de> Visitor<'de> for StringOrReplaceRulesVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON string or YAML sequence of replace rules")
    }

    // Handle existing JSON strings
    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        // Validate it's at least valid JSON
        let _: serde_json::Value =
            serde_json::from_str(value).map_err(|_| E::custom("Expected valid JSON string"))?;
        Ok(value.to_string())
    }

    // Convert YAML sequences to JSON strings
    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut rules = Vec::new();
        while let Some(rule) = seq.next_element::<ReplaceRuleYaml>()? {
            rules.push(rule);
        }
        // Serialize to JSON string for compatibility with parse_rules_from_string
        serde_json::to_string(&rules).map_err(|e| {
            serde::de::Error::custom(format!("Failed to serialize rules to JSON: {e}"))
        })
    }
}

pub fn deserialize_apm_replace_rules<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<ReplaceRule>>, D::Error>
where
    D: Deserializer<'de>,
{
    let json_string = deserializer.deserialize_any(StringOrReplaceRulesVisitor)?;

    let rules = parse_rules_from_string(&json_string)
        .map_err(|e| serde::de::Error::custom(format!("Parse error: {e}")))?;

    Ok(Some(rules))
}
