use crate::config::flush_strategy::FlushStrategy;
use std::time;
use tokio::time::Interval;
use tracing::debug;

use crate::lifecycle::invocation_times::InvocationTimes;

const DEFAULT_FLUSH_INTERVAL: u64 = 60 * 1000; // 60s
const TWENTY_SECONDS: u64 = 20 * 1000;
const FIFTEEN_MINUTES: u64 = 15 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlushControl {
    pub last_flush: u64,
    pub flush_strategy: FlushStrategy,
    invocation_times: InvocationTimes,
}

// 1. Default Strategy
//   - Flush every 1s and at the end of the invocation
//  2. Periodic Strategy
//      - User specifies the interval in milliseconds, will not block on the runtimeDone event
//  3. End strategy
//      - Always flush at the end of the invocation
impl FlushControl {
    #[must_use]
    pub fn new(flush_strategy: FlushStrategy) -> FlushControl {
        FlushControl {
            flush_strategy,
            last_flush: 0,
            invocation_times: InvocationTimes::new(),
        }
    }

    #[must_use]
    pub fn should_flush_end(&mut self) -> bool {
        // previously: would return true if flush_strategy is not Periodically
        // !matches!(self.flush_strategy, FlushStrategy::Periodically(_))
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs();
        self.invocation_times.add(now);
        match &self.flush_strategy {
            FlushStrategy::End | FlushStrategy::EndPeriodically(_) => true,
            FlushStrategy::Periodically(_) => false,
            FlushStrategy::Default => {
                if self.invocation_times.should_adapt_to_periodic(now) {
                    return false;
                }
                debug!("Not enough invocations to adapt to periodic flush, flushing at the end of the invocation");
                self.last_flush = now;
                true
            }
        }
    }

    #[must_use]
    pub fn get_flush_interval(&self) -> Interval {
        match &self.flush_strategy {
            FlushStrategy::Default => {
                tokio::time::interval(tokio::time::Duration::from_millis(DEFAULT_FLUSH_INTERVAL))
            }
            FlushStrategy::Periodically(p) | FlushStrategy::EndPeriodically(p) => {
                tokio::time::interval(tokio::time::Duration::from_millis(p.interval))
            }
            FlushStrategy::End => {
                tokio::time::interval(tokio::time::Duration::from_millis(FIFTEEN_MINUTES))
            }
        }
    }

    // Only used for default strategy
    #[must_use]
    pub fn should_periodic_flush(&mut self) -> bool {
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs();
        match &self.flush_strategy {
            FlushStrategy::Default | FlushStrategy::Periodically(_) => {
                let interval = match &self.flush_strategy {
                    FlushStrategy::Default => TWENTY_SECONDS,
                    FlushStrategy::Periodically(strategy) => strategy.interval,
                    _ => return false,
                };

                if now - self.last_flush > interval / 1000 {
                    self.last_flush = now;
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flush_strategy::PeriodicStrategy;

    #[test]
    fn should_flush_end() {
        let mut flush_control = FlushControl::new(FlushStrategy::Default);
        assert!(flush_control.should_flush_end());

        let mut flush_control =
            FlushControl::new(FlushStrategy::EndPeriodically(PeriodicStrategy {
                interval: 1,
            }));
        assert!(flush_control.should_flush_end());

        let mut flush_control = FlushControl::new(FlushStrategy::End);
        assert!(flush_control.should_flush_end());

        let mut flush_control = FlushControl::new(FlushStrategy::Periodically(PeriodicStrategy {
            interval: 1,
        }));
        assert!(!flush_control.should_flush_end());
    }

    #[test]
    fn should_flush_default_end() {
        let mut flush_control = super::FlushControl::new(FlushStrategy::Default);
        assert!(flush_control.should_flush_end());
    }

    #[test]
    fn should_flush_default_periodic() {
        const LOOKBACK_COUNT: usize = 20;
        let mut flush_control = super::FlushControl::new(FlushStrategy::Default);
        for _ in 0..LOOKBACK_COUNT - 1 {
            assert!(flush_control.should_flush_end());
        }
        assert!(!flush_control.should_flush_end());
    }

    #[test]
    fn should_flush_custom_periodic() {
        let mut flush_control =
            super::FlushControl::new(FlushStrategy::Periodically(PeriodicStrategy {
                interval: 1,
            }));
        assert!(flush_control.should_periodic_flush());
    }

    #[tokio::test]
    async fn get_flush_interval() {
        let flush_control = FlushControl::new(FlushStrategy::Default);
        assert_eq!(
            flush_control.get_flush_interval().period().as_millis(),
            u128::from(DEFAULT_FLUSH_INTERVAL)
        );

        let flush_control = FlushControl::new(FlushStrategy::Periodically(PeriodicStrategy {
            interval: 1,
        }));
        assert_eq!(flush_control.get_flush_interval().period().as_millis(), 1);

        let flush_control = FlushControl::new(FlushStrategy::EndPeriodically(PeriodicStrategy {
            interval: 1,
        }));
        assert_eq!(flush_control.get_flush_interval().period().as_millis(), 1);

        let flush_control = FlushControl::new(FlushStrategy::End);
        assert_eq!(
            flush_control.get_flush_interval().period().as_millis(),
            tokio::time::Duration::from_millis(FIFTEEN_MINUTES).as_millis()
        );
    }
}
