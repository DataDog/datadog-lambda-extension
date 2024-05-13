use serde::{Deserialize, Deserializer};
use tracing::debug;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeriodicStrategy {
    pub interval: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FlushStrategy {
    Default,
    End,
    Periodically(PeriodicStrategy),
}

// Deserialize for FlushStrategy
// Flush Strategy can be either "end" or "periodically,<ms>"
impl<'de> Deserialize<'de> for FlushStrategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.as_str() == "end" {
            Ok(FlushStrategy::End)
        } else {
            let mut split_value = value.as_str().split(',');
            // "periodically,60000"
            match split_value.next() {
                Some(first_value) if first_value.starts_with("periodically") => {
                    let interval = split_value.next();
                    // "60000"
                    if let Some(interval) = interval {
                        if let Ok(parsed_interval) = interval.parse() {
                            return Ok(FlushStrategy::Periodically(PeriodicStrategy {
                                interval: parsed_interval,
                            }));
                        }
                        debug!("invalid flush interval: {}, using default", interval);
                        Ok(FlushStrategy::Default)
                    } else {
                        debug!("invalid flush strategy: {}, using default", value);
                        Ok(FlushStrategy::Default)
                    }
                }
                _ => {
                    debug!("invalid flush strategy: {}, using default", value);
                    Ok(FlushStrategy::Default)
                }
            }
        }
    }
}
