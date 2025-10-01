use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

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
        .filter_map(|pair| {
            let mut split = pair.split(':');

            let service = split.next();
            let to_map = split.next();

            if let (Some(service), Some(to_map)) = (service, to_map) {
                Some((service.trim().to_string(), to_map.trim().to_string()))
            } else {
                tracing::error!("Failed to parse service mapping '{}', expected format 'service:mapped_service', ignoring", pair.trim());
                None
            }
        })
        .collect();

    Ok(map)
}
