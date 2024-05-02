use::std::time;
use crate::config::FlushStrategy;
use crate::lifecycle::invocation_times::InvocationTimes;
use tracing::debug;

const TWENTY_SECONDS: u64 = 20 * 1000;

pub struct FlushControl {
    pub last_flush: u64,
    flush_strategy: FlushStrategy,
    invocation_times: InvocationTimes,
}

impl FlushControl {
    pub fn new(flush_strategy: FlushStrategy) -> FlushControl {
        FlushControl { flush_strategy, last_flush: 0, invocation_times: InvocationTimes::new() }
    }

    pub fn should_flush(&mut self) -> bool {
        let now = time::SystemTime::now().duration_since(time::UNIX_EPOCH).unwrap().as_secs();
        self.invocation_times.add(now);
        match &self.flush_strategy {
            FlushStrategy::Default => {
                if self.invocation_times.should_adapt_to_periodic(now) {
                    let should_periodic_flush = self.should_periodic_flush(TWENTY_SECONDS);
                    debug!("Adapting over to periodic flush strategy. should_periodic_flush: {}", should_periodic_flush);
                    return should_periodic_flush;
                }
                debug!("Not enough invocations to adapt to periodic flush, flushing at the end of the invocation");
                self.last_flush = now;
                true
            },
            FlushStrategy::Periodically(periodic) => self.should_periodic_flush(periodic.interval),
            FlushStrategy::End => true,
        }
    }

    fn should_periodic_flush(&mut self, interval: u64) -> bool {
        let now = time::SystemTime::now().duration_since(time::UNIX_EPOCH).unwrap().as_secs();
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
        assert_eq!(flush_control.should_flush(), true);
    }
    #[test]
    fn should_flush_default_periodic() {
        const LOOKBACK_COUNT: usize = 20;
        let mut flush_control = super::FlushControl::new(FlushStrategy::Default);
        for _ in 0..LOOKBACK_COUNT-1 {
            assert_eq!(flush_control.should_flush(), true);
        }
            assert_eq!(flush_control.should_flush(), false);
    }
    #[test]
    fn should_flush_end() {
        let mut flush_control = super::FlushControl::new(FlushStrategy::End);
        assert_eq!(flush_control.should_flush(), true);
    }
    #[test]
    fn should_flush_periodically() {
        let mut flush_control = super::FlushControl::new(FlushStrategy::Periodically(config::PeriodicStrategy{ interval: 1 }));
        assert_eq!(flush_control.should_flush(), true);
        assert_eq!(flush_control.should_flush(), false);
    }
}
