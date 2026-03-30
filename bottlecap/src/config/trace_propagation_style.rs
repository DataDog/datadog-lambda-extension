use std::str::FromStr;

use datadog_opentelemetry::propagation::TracePropagationStyle;
use serde::{Deserialize, Deserializer};
use tracing::error;

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
                    error!("Failed to parse trace propagation style: {}, ignoring", e);
                    None
                }
            },
        )
        .collect())
}
