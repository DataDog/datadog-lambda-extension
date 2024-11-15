use std::collections::HashMap;

use serde::{Deserialize, Deserializer};
use tracing::debug;

#[allow(clippy::module_name_repetitions)]
pub fn deserialize_service_mapping<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = String::deserialize(deserializer)?;

    let map = s
        .split(',')
        .map(|pair| {
            let mut split = pair.split(':');

            let service = split.next();
            let to_map = split.next();

            if let (Some(service), Some(to_map)) = (service, to_map) {
                Ok((service.trim().to_string(), to_map.trim().to_string()))
            } else {
                debug!("Ignoring invalid service mapping pair: {pair}");
                Err(serde::de::Error::custom(format!(
                    "Failed to deserialize service mapping for pair: {pair}"
                )))
            }
        })
        .collect();

    map
}
