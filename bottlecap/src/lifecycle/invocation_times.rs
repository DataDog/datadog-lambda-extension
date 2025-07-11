use crate::config::flush_strategy::{ConcreteFlushStrategy, PeriodicStrategy};

const TWENTY_SECONDS: u64 = 20 * 1000;
const LOOKBACK_COUNT: usize = 20;
const ONE_TWENTY_SECONDS: f64 = 120.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InvocationTimes {
    times: [u64; LOOKBACK_COUNT],
    head: usize,
}

impl InvocationTimes {
    pub(crate) fn new() -> InvocationTimes {
        InvocationTimes {
            times: [0; LOOKBACK_COUNT],
            head: 0,
        }
    }

    pub(crate) fn add(&mut self, timestamp: u64) {
        self.times[self.head] = timestamp;
        self.head = (self.head + 1) % LOOKBACK_COUNT;
    }

    // Translate FlushStrategy::Default to a ConcreteFlushStrategy, based on past invocation times.
    pub(crate) fn evaluate_default_strategy(
        &self,
        now: u64,
        flush_timeout: u64,
    ) -> ConcreteFlushStrategy {
        // If the buffer isn't full, then we haven't seen enough invocations, so we should flush
        // at the end of the invocation.
        for idx in self.head..LOOKBACK_COUNT {
            if self.times[idx] == 0 {
                return ConcreteFlushStrategy::End;
            }
        }

        // Now we've seen at least 20 invocations. Possible cases:
        // 1. If the average time between invocations is longer than 2 minutes, stick to End strategy.
        // 2. If average interval is shorter than 2 minutes:
        //   2.1 If it's very short, use the continuous strategy to minimize delaying the next invocation.
        //   2.2 If it's not too short, use the periodic strategy to minimize the risk that
        //       flushing is delayed due to the Lambda environment being frozen between invocations.
        // We get the average time between each invocation by taking the difference between newest (`now`) and the
        // oldest invocation in the buffer, then dividing by `LOOKBACK_COUNT - 1`.
        let oldest = self.times[self.head];

        let elapsed = now - oldest;
        let should_adapt = (elapsed as f64 / (LOOKBACK_COUNT - 1) as f64) < ONE_TWENTY_SECONDS;
        if should_adapt {
            // Both units here are in seconds
            // TODO: What does this mean?
            if elapsed < flush_timeout {
                return ConcreteFlushStrategy::Continuously(PeriodicStrategy {
                    interval: TWENTY_SECONDS,
                });
            }
            return ConcreteFlushStrategy::Periodically(PeriodicStrategy {
                interval: TWENTY_SECONDS,
            });
        }
        ConcreteFlushStrategy::End
    }
}

#[cfg(test)]
mod tests {
    use crate::config::flush_strategy::{ConcreteFlushStrategy, PeriodicStrategy};
    use crate::lifecycle::invocation_times::{self, TWENTY_SECONDS};

    #[test]
    fn new() {
        let invocation_times = invocation_times::InvocationTimes::new();
        assert_eq!(
            invocation_times.times,
            [0; invocation_times::LOOKBACK_COUNT]
        );
        assert_eq!(invocation_times.head, 0);
    }

    #[test]
    fn insertion() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        let timestamp = 1;
        invocation_times.add(timestamp);
        assert_eq!(invocation_times.times[0], timestamp);
        assert_eq!(invocation_times.head, 1);
        assert_eq!(
            invocation_times.evaluate_default_strategy(1, 60),
            ConcreteFlushStrategy::End
        );
    }

    #[test]
    fn insertion_with_full_buffer_fast_invokes() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        for i in 0..=invocation_times::LOOKBACK_COUNT {
            invocation_times.add(i as u64);
        }
        // should wrap around
        assert_eq!(invocation_times.times[0], 20);
        assert_eq!(invocation_times.head, 1);
        assert_eq!(
            invocation_times.evaluate_default_strategy(21, 60),
            ConcreteFlushStrategy::Continuously(PeriodicStrategy {
                interval: TWENTY_SECONDS
            })
        );
    }

    #[test]
    fn insertion_with_full_buffer_fast_invokes_low_timeout() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        for i in 0..=invocation_times::LOOKBACK_COUNT {
            invocation_times.add(i as u64);
        }
        // should wrap around
        assert_eq!(invocation_times.times[0], 20);
        assert_eq!(invocation_times.head, 1);
        assert_eq!(
            invocation_times.evaluate_default_strategy(21, 1),
            ConcreteFlushStrategy::Periodically(PeriodicStrategy {
                interval: TWENTY_SECONDS
            })
        );
    }

    #[test]
    fn insertion_with_full_buffer_slow_invokes() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        invocation_times.add(1_u64);
        for i in 0..invocation_times::LOOKBACK_COUNT {
            invocation_times.add((i + 5000) as u64);
        }
        // should wrap around
        assert_eq!(invocation_times.times[0], 5019);
        assert_eq!(invocation_times.head, 1);
        assert_eq!(
            invocation_times.evaluate_default_strategy(10000, 60),
            ConcreteFlushStrategy::End
        );
    }

    #[test]
    fn should_adapt_to_periodic_when_fast_invokes() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        for i in 0..=(invocation_times::LOOKBACK_COUNT + 5) {
            invocation_times.add((i * 100 + 1) as u64);
        }

        assert_eq!(invocation_times.times[0], 2001);
        assert_eq!(invocation_times.times[5], 2501);
        assert_eq!(invocation_times.times[6], 601);
        assert_eq!(
            invocation_times.times[invocation_times::LOOKBACK_COUNT - 1],
            1901
        );
        assert_eq!(
            invocation_times.evaluate_default_strategy(2501, 60),
            ConcreteFlushStrategy::Periodically(PeriodicStrategy {
                interval: TWENTY_SECONDS
            })
        );
    }

    #[test]
    fn should_not_adapt_to_periodic_when_slow_invokes() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        for i in 0..=(invocation_times::LOOKBACK_COUNT + 5) {
            invocation_times.add((i * 130 + 1) as u64);
        }

        assert_eq!(invocation_times.times[0], 2601);
        assert_eq!(invocation_times.times[5], 3251);
        assert_eq!(invocation_times.times[6], 781);
        assert_eq!(
            invocation_times.times[invocation_times::LOOKBACK_COUNT - 1],
            2471
        );
        assert_eq!(
            invocation_times.evaluate_default_strategy(3251, 60),
            ConcreteFlushStrategy::End
        );
    }
}
