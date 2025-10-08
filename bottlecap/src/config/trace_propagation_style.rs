use std::{fmt::Display, str::FromStr};

use serde::{Deserialize, Deserializer};
use tracing::error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracePropagationStyle {
    Datadog,
    B3Multi,
    B3,
    TraceContext,
    None,
}

impl FromStr for TracePropagationStyle {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "datadog" => Ok(TracePropagationStyle::Datadog),
            "b3multi" => Ok(TracePropagationStyle::B3Multi),
            "b3" => Ok(TracePropagationStyle::B3),
            "tracecontext" => Ok(TracePropagationStyle::TraceContext),
            "none" => Ok(TracePropagationStyle::None),
            _ => {
                error!("Trace propagation style is invalid: {:?}, using Datadog", s);
                Ok(TracePropagationStyle::Datadog)
            }
        }
    }
}

impl Display for TracePropagationStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let style = match self {
            TracePropagationStyle::Datadog => "datadog",
            TracePropagationStyle::B3Multi => "b3multi",
            TracePropagationStyle::B3 => "b3",
            TracePropagationStyle::TraceContext => "tracecontext",
            TracePropagationStyle::None => "none",
        };
        write!(f, "{style}")
    }
}

#[allow(clippy::module_name_repetitions)]
pub fn deserialize_trace_propagation_style<'de, D>(
    deserializer: D,
) -> Result<Vec<TracePropagationStyle>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = String::deserialize(deserializer)?;

    Ok(s.split(',')
        .filter_map(
            |style| match TracePropagationStyle::from_str(style.trim()) {
                Ok(parsed_style) => Some(parsed_style),
                Err(e) => {
                    tracing::error!("Failed to parse trace propagation style: {}, ignoring", e);
                    None
                }
            },
        )
        .collect())
}
