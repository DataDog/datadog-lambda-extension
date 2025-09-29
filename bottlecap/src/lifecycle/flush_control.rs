use crate::config::flush_strategy::{ConcreteFlushStrategy, FlushStrategy};
use std::time;
use tokio::time::{Interval, MissedTickBehavior::Skip};

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

// The flush behavior for the current moment
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FlushDecision {
    Continuous,
    Periodic,
    End,
    Dont,
}

impl FlushControl {
    #[must_use]
    pub fn new(flush_strategy: FlushStrategy, flush_timeout: u64) -> FlushControl {
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs();
        FlushControl {
            flush_strategy,
            last_flush: now,
            invocation_times: InvocationTimes::new(),
            flush_timeout,
        }
    }

    #[must_use]
    pub fn get_flush_interval(&self) -> Interval {
        match &self.flush_strategy {
            FlushStrategy::Default => {
                let mut i = tokio::time::interval(tokio::time::Duration::from_millis(
                    DEFAULT_FLUSH_INTERVAL,
                ));
                i.set_missed_tick_behavior(Skip);
                i
            }
            FlushStrategy::Periodically(p)
            | FlushStrategy::EndPeriodically(p)
            | FlushStrategy::Continuously(p) => {
                let mut i = tokio::time::interval(tokio::time::Duration::from_millis(p.interval));
                i.set_missed_tick_behavior(Skip);
                i
            }
            FlushStrategy::End => {
                // Set the race flush interval to the maximum value of Lambda timeout, so flush will
                // only happen at the end of the invocation, and race flush will never happen.
                tokio::time::interval(tokio::time::Duration::from_millis(FIFTEEN_MINUTES))
            }
        }
    }

    // Evaluate the flush decision for the current moment, based on the flush strategy, current time,
    // and the past invocation times.
    #[must_use]
    pub fn evaluate_flush_decision(&mut self) -> FlushDecision {
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs();
        self.invocation_times.add(now);
        let concrete_flush_strategy = self.invocation_times.evaluate_concrete_strategy(
            now,
            self.flush_timeout,
            self.flush_strategy,
        );
        match concrete_flush_strategy {
            ConcreteFlushStrategy::Periodically(strategy) => {
                if self.interval_passed(now, strategy.interval) {
                    self.last_flush = now;
                    // TODO calculate periodic rate. if it's more frequent than the flush_timeout
                    // opt in to continuous
                    FlushDecision::Periodic
                } else {
                    FlushDecision::Dont
                }
            }
            ConcreteFlushStrategy::Continuously(strategy) => {
                if self.interval_passed(now, strategy.interval) {
                    self.last_flush = now;
                    // TODO calculate periodic rate. if it's more frequent than the flush_timeout
                    // opt in to continuous
                    FlushDecision::Continuous
                } else {
                    FlushDecision::Dont
                }
            }
            ConcreteFlushStrategy::End => FlushDecision::End,
            ConcreteFlushStrategy::EndPeriodically(strategy) => {
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

    #[test]
    fn should_flush_end() {
        let mut flush_control = FlushControl::new(FlushStrategy::Default, 60);
        assert_eq!(flush_control.evaluate_flush_decision(), FlushDecision::End);

        let mut flush_control = FlushControl::new(
            FlushStrategy::EndPeriodically(PeriodicStrategy { interval: 1 }),
            60,
        );
        // Set last_flush to an older time to trigger flush
        flush_control.last_flush = flush_control.last_flush - 2;
        assert_eq!(flush_control.evaluate_flush_decision(), FlushDecision::End);

        let mut flush_control = FlushControl::new(FlushStrategy::End, 60);
        assert_eq!(flush_control.evaluate_flush_decision(), FlushDecision::End);

        let mut flush_control = FlushControl::new(
            FlushStrategy::Periodically(PeriodicStrategy { interval: 1 }),
            60,
        );
        // Set last_flush to an older time to trigger flush
        flush_control.last_flush = flush_control.last_flush - 2;
        assert_eq!(
            flush_control.evaluate_flush_decision(),
            FlushDecision::Periodic
        );
    }

    #[test]
    fn should_flush_default_end() {
        let mut flush_control = super::FlushControl::new(FlushStrategy::Default, 60);
        assert_eq!(flush_control.evaluate_flush_decision(), FlushDecision::End);
    }

    #[test]
    fn should_flush_default_continuous() {
        const LOOKBACK_COUNT: usize = 20;
        let mut flush_control = super::FlushControl::new(FlushStrategy::Default, 60);
        for _ in 0..LOOKBACK_COUNT - 1 {
            assert_eq!(flush_control.evaluate_flush_decision(), FlushDecision::End);
        }
        // Set last_flush to an older time to trigger flush after lookback threshold
        flush_control.last_flush = flush_control.last_flush - 61;
        assert_eq!(
            flush_control.evaluate_flush_decision(),
            FlushDecision::Continuous
        );
    }

    #[test]
    fn should_flush_custom_periodic() {
        let mut flush_control = super::FlushControl::new(
            FlushStrategy::Periodically(PeriodicStrategy { interval: 1 }),
            60,
        );
        // Set last_flush to an older time to trigger flush
        flush_control.last_flush = flush_control.last_flush - 2;
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
