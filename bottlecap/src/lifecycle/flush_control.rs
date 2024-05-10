use crate::config::FlushStrategy;
use crate::lifecycle::invocation_times::InvocationTimes;
use ::std::time;
use tracing::debug;

const TWENTY_SECONDS: u64 = 20 * 1000;

pub struct FlushControl {
    pub last_flush: u64,
    flush_strategy: FlushStrategy,
    invocation_times: InvocationTimes,
}

// FlushControl is called at the end of every invocation and decides whether or not we should flush
// The flushing logic is complex and depends on the flush strategy
// 1. Default Strategy
//   - Flush at the end of the first 20 invocations
//   - We keep track of the last 20 invocations and calculate the frequency.
//     - If the duration from the last invocation to the 20th is less than 2 minutes, switch to
//     periodic flush every 20s
//     - else, flush at the end of the invocation
//  2. Periodic Strategy
//      - User specifies the interval in milliseconds
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

    pub fn should_flush(&mut self) -> bool {
        let now = match time::SystemTime::now().duration_since(time::UNIX_EPOCH) {
            Ok(now) => now.as_secs(),
            Err(e) => {
                debug!("failed to get current time: {:?}", e);
                return false;
            }
        };
        self.invocation_times.add(now);
        match &self.flush_strategy {
            FlushStrategy::Default => {
                if self.invocation_times.should_adapt_to_periodic(now) {
                    let should_periodic_flush = self.should_periodic_flush(now, TWENTY_SECONDS);
                    debug!(
                        "Adapting over to periodic flush strategy. should_periodic_flush: {}",
                        should_periodic_flush
                    );
                    return should_periodic_flush;
                }
                debug!("Not enough invocations to adapt to periodic flush, flushing at the end of the invocation");
                self.last_flush = now;
                true
            }
            FlushStrategy::Periodically(periodic) => {
                self.should_periodic_flush(now, periodic.interval)
            }
            FlushStrategy::End => true,
        }
    }

    fn should_periodic_flush(&mut self, now: u64, interval: u64) -> bool {
        if now - self.last_flush > (interval / 1000) {
            self.last_flush = now;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{self, FlushStrategy};
    #[test]
    fn should_flush_default_end() {
        let mut flush_control = super::FlushControl::new(FlushStrategy::Default);
        assert!(flush_control.should_flush());
    }
    #[test]
    fn should_flush_default_periodic() {
        const LOOKBACK_COUNT: usize = 20;
        let mut flush_control = super::FlushControl::new(FlushStrategy::Default);
        for _ in 0..LOOKBACK_COUNT - 1 {
            assert!(flush_control.should_flush());
        }
        assert!(!flush_control.should_flush());
    }
    #[test]
    fn should_flush_end() {
        let mut flush_control = super::FlushControl::new(FlushStrategy::End);
        assert!(flush_control.should_flush());
    }
    #[test]
    fn should_flush_periodically() {
        let mut flush_control =
            super::FlushControl::new(FlushStrategy::Periodically(config::PeriodicStrategy {
                interval: 1,
            }));
        assert!(flush_control.should_flush());
        assert!(!flush_control.should_flush());
    }
}
