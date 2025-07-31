use std::collections::HashMap;
use std::hash::Hash;

use serde::de::{Deserialize, Deserializer};

/// Deserialize a map, turning `null` into [`HashMap::default`].
pub(super) fn nullable_map<
    'de,
    K: Deserialize<'de> + Eq + Hash,
    V: Deserialize<'de>,
    D: Deserializer<'de>,
>(
    deserializer: D,
) -> Result<HashMap<K, V>, D::Error> {
    let maybe = Option::deserialize(deserializer)?;
    Ok(maybe.unwrap_or_default())
}
