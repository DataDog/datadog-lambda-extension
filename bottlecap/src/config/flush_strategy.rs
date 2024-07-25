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
            let strategy = split_value.next();
            let interval: Option<u64> = split_value.next().and_then(|v| v.parse().ok());

            match (strategy, interval) {
                (Some("periodically"), Some(interval)) => {
                    Ok(FlushStrategy::Periodically(PeriodicStrategy { interval }))
                }
                (Some("end"), Some(interval)) => {
                    Ok(FlushStrategy::EndPeriodically(PeriodicStrategy {
                        interval,
                    }))
                }
                (Some(strategy), _) => {
                    debug!("Invalid flush interval: {}, using default", strategy);
                    Ok(FlushStrategy::Default)
                }
                _ => {
                    debug!("Invalid flush strategy: {}, using default", value);
                    Ok(FlushStrategy::Default)
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_end() {
        let flush_strategy: FlushStrategy = serde_json::from_str("\"end\"").unwrap();
        assert_eq!(flush_strategy, FlushStrategy::End);
    }

    #[test]
    fn deserialize_periodically() {
        let flush_strategy: FlushStrategy = serde_json::from_str("\"periodically,60000\"").unwrap();
        assert_eq!(
            flush_strategy,
            FlushStrategy::Periodically(PeriodicStrategy { interval: 60000 })
        );
    }

    #[test]
    fn deserialize_end_periodically() {
        let flush_strategy: FlushStrategy = serde_json::from_str("\"end,1000\"").unwrap();
        assert_eq!(
            flush_strategy,
            FlushStrategy::EndPeriodically(PeriodicStrategy { interval: 1000 })
        );
    }

    #[test]
    fn deserialize_invalid() {
        let flush_strategy: FlushStrategy = serde_json::from_str("\"invalid\"").unwrap();
        assert_eq!(flush_strategy, FlushStrategy::Default);
    }

    #[test]
    fn deserialize_invalid_interval() {
        let flush_strategy: FlushStrategy =
            serde_json::from_str("\"periodically,invalid\"").unwrap();
        assert_eq!(flush_strategy, FlushStrategy::Default);
    }

    #[test]
    fn deserialize_invalid_end_interval() {
        let flush_strategy: FlushStrategy = serde_json::from_str("\"end,invalid\"").unwrap();
        assert_eq!(flush_strategy, FlushStrategy::Default);
    }
}
