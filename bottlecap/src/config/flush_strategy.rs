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
    EndPeriodically(PeriodicStrategy),
    Periodically(PeriodicStrategy),
}

// Deserialize for FlushStrategy
// Flush Strategy can be either "end", "end,<ms>", or "periodically,<ms>"
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
            // "end,1000"
            //
            // "periodically|end"
            if let Some(strategy) = split_value.next() {
                // "60000"
                let mut interval = 0;
                if let Some(v) = split_value.next() {
                    if let Ok(parsed_interval) = v.parse() {
                        interval = parsed_interval;
                    }
                }

                if interval == 0 {
                    debug!("invalid flush interval: {}, using default", value);
                    return Ok(FlushStrategy::Default);
                }

                return match strategy {
                    "periodically" => {
                        Ok(FlushStrategy::Periodically(PeriodicStrategy { interval }))
                    }
                    "end" => Ok(FlushStrategy::EndPeriodically(PeriodicStrategy {
                        interval,
                    })),
                    _ => {
                        debug!("invalid flush strategy: {}, using default", value);
                        Ok(FlushStrategy::Default)
                    }
                };
            }

            debug!("invalid flush strategy: {}, using default", value);
            Ok(FlushStrategy::Default)
        }
    }
}
