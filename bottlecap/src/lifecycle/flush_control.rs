use crate::config::flush_strategy::FlushStrategy;
use std::time;
use tokio::time::Interval;

use crate::lifecycle::invocation_times::InvocationTimes;

const DEFAULT_FLUSH_INTERVAL: u64 = 60 * 1000; // 60s
const FIFTEEN_MINUTES: u64 = 15 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlushControl {
    pub last_flush: u64,
    pub flush_strategy: FlushStrategy,
    invocation_times: InvocationTimes,
    flush_timeout: u64,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FlushDecision {
    Continuous,
    Periodic,
    End,
    Dont,
}

// 1. Default Strategy
//   - Flush every 1s and at the end of the invocation
//  2. Periodic Strategy
//      - User specifies the interval in milliseconds, will not block on the runtimeDone event
//  3. End strategy
//      - Always flush at the end of the invocation
impl FlushControl {
    #[must_use]
    pub fn new(flush_strategy: FlushStrategy, flush_timeout: u64) -> FlushControl {
        FlushControl {
            flush_strategy,
            last_flush: 0,
            invocation_times: InvocationTimes::new(),
            flush_timeout,
        }
    }

    #[must_use]
    pub fn get_flush_interval(&self) -> Interval {
        match &self.flush_strategy {
            FlushStrategy::Default => {
                tokio::time::interval(tokio::time::Duration::from_millis(DEFAULT_FLUSH_INTERVAL))
            }
            FlushStrategy::Periodically(p)
            | FlushStrategy::EndPeriodically(p)
            | FlushStrategy::Continuously(p) => {
                tokio::time::interval(tokio::time::Duration::from_millis(p.interval))
            }
            FlushStrategy::End => {
                tokio::time::interval(tokio::time::Duration::from_millis(FIFTEEN_MINUTES))
            }
        }
    }

    #[must_use]
    pub fn evaluate_flush_decision(&mut self) -> FlushDecision {
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs();
        self.invocation_times.add(now);
        let evaluated_flush_strategy = if self.flush_strategy == FlushStrategy::Default {
            &self.invocation_times.should_adapt(now, self.flush_timeout)
        } else {
            // User specified one
            &self.flush_strategy
        };
        match evaluated_flush_strategy {
            FlushStrategy::Default => {
                unreachable!("should_adapt must translate default strategy to concrete strategy")
            }
            FlushStrategy::Periodically(strategy) => {
                if self.interval_passed(now, strategy.interval) {
                    self.last_flush = now;
                    // TODO calculate periodic rate. if it's more frequent than the flush_timeout
                    // opt in to continuous
                    FlushDecision::Periodic
                } else {
                    FlushDecision::Dont
                }
            }
            FlushStrategy::Continuously(strategy) => {
                if self.interval_passed(now, strategy.interval) {
                    self.last_flush = now;
                    // TODO calculate periodic rate. if it's more frequent than the flush_timeout
                    // opt in to continuous
                    FlushDecision::Continuous
                } else {
                    FlushDecision::Dont
                }
            }
            FlushStrategy::End => FlushDecision::End,
            FlushStrategy::EndPeriodically(strategy) => {
                if self.interval_passed(now, strategy.interval) {
                    self.last_flush = now;
                    FlushDecision::End
                } else {
                    FlushDecision::Dont
                }
            }
        }
    }

    fn interval_passed(&self, now: u64, interval: u64) -> bool {
        now - self.last_flush > interval / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flush_strategy::PeriodicStrategy;

    // #[test]
    // fn should_flush_end() {
    //     let mut flush_control = FlushControl::new(FlushStrategy::Default, 60);
    //     assert!(flush_control.should_flush_end());

    //     let mut flush_control = FlushControl::new(
    //         FlushStrategy::EndPeriodically(PeriodicStrategy { interval: 1 }),
    //         60,
    //     );
    //     assert!(flush_control.should_flush_end());

    //     let mut flush_control = FlushControl::new(FlushStrategy::End, 60);
    //     assert!(flush_control.should_flush_end());

    //     let mut flush_control = FlushControl::new(
    //         FlushStrategy::Periodically(PeriodicStrategy { interval: 1 }),
    //         60,
    //     );
    //     assert!(!flush_control.should_flush_end());
    // }

    // #[test]
    // fn should_flush_default_end() {
    //     let mut flush_control = super::FlushControl::new(FlushStrategy::Default, 60);
    //     assert!(flush_control.should_flush_end());
    // }

    // #[test]
    // fn should_flush_default_periodic() {
    //     const LOOKBACK_COUNT: usize = 20;
    //     let mut flush_control = super::FlushControl::new(FlushStrategy::Default, 60);
    //     for _ in 0..LOOKBACK_COUNT - 1 {
    //         assert!(flush_control.should_flush_end());
    //     }
    //     assert!(!flush_control.should_flush_end());
    // }

    #[test]
    fn should_flush_custom_periodic() {
        let mut flush_control = super::FlushControl::new(
            FlushStrategy::Periodically(PeriodicStrategy { interval: 1 }),
            60,
        );
        assert_eq!(
            flush_control.evaluate_flush_decision(),
            FlushDecision::Periodic,
        );
    }

    #[tokio::test]
    async fn get_flush_interval() {
        let flush_control = FlushControl::new(FlushStrategy::Default, 60);
        assert_eq!(
            flush_control.get_flush_interval().period().as_millis(),
            u128::from(DEFAULT_FLUSH_INTERVAL)
        );

        let flush_control = FlushControl::new(
            FlushStrategy::Periodically(PeriodicStrategy { interval: 1 }),
            60,
        );
        assert_eq!(flush_control.get_flush_interval().period().as_millis(), 1);

        let flush_control = FlushControl::new(
            FlushStrategy::EndPeriodically(PeriodicStrategy { interval: 1 }),
            60,
        );
        assert_eq!(flush_control.get_flush_interval().period().as_millis(), 1);

        let flush_control = FlushControl::new(FlushStrategy::End, 60);
        assert_eq!(
            flush_control.get_flush_interval().period().as_millis(),
            tokio::time::Duration::from_millis(FIFTEEN_MINUTES).as_millis()
        );
    }
}
