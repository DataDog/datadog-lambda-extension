use datadog_trace_obfuscation::replacer::{ReplaceRule, parse_rules_from_string};
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
        match serde_json::from_str::<serde_json::Value>(value) {
            Ok(_) => Ok(value.to_string()),
            Err(e) => {
                tracing::error!("Invalid JSON string for APM replace rules: {}", e);
                Ok(String::new())
            }
        }
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
        match serde_json::to_string(&rules) {
            Ok(json) => Ok(json),
            Err(e) => {
                tracing::error!("Failed to convert YAML rules to JSON: {}", e);
                Ok(String::new())
            }
        }
    }
}

pub fn deserialize_apm_replace_rules<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<ReplaceRule>>, D::Error>
where
    D: Deserializer<'de>,
{
    let json_string = deserializer.deserialize_any(StringOrReplaceRulesVisitor)?;

    match parse_rules_from_string(&json_string) {
        Ok(rules) => Ok(Some(rules)),
        Err(e) => {
            tracing::error!("Failed to parse APM replace rule, ignoring: {}", e);
            Ok(None)
        }
    }
}
