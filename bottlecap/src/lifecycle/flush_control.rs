use crate::config::flush_strategy::FlushStrategy;
use tokio::time::Interval;

const DEFAULT_FLUSH_INTERVAL: u64 = 1000; // 1s

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlushControl {
    flush_strategy: FlushStrategy,
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
        FlushControl { flush_strategy }
    }

    #[must_use]
    pub fn should_flush_end(&self) -> bool {
        !matches!(&self.flush_strategy, FlushStrategy::Periodically(_))
    }

    #[must_use]
    pub fn get_flush_interval(&self) -> Interval {
        match &self.flush_strategy {
            FlushStrategy::Default => {
                tokio::time::interval(tokio::time::Duration::from_millis(DEFAULT_FLUSH_INTERVAL))
            }
            FlushStrategy::Periodically(p) => {
                tokio::time::interval(tokio::time::Duration::from_millis(p.interval))
            }
            FlushStrategy::End => tokio::time::interval(tokio::time::Duration::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::flush_strategy::PeriodicStrategy;

    #[test]
    fn should_flush_end() {
        let flush_control = FlushControl::new(FlushStrategy::Default);
        assert!(flush_control.should_flush_end());

        let flush_control = FlushControl::new(FlushStrategy::Periodically(PeriodicStrategy {
            interval: 1,
        }));
        assert!(!flush_control.should_flush_end());

        let flush_control = FlushControl::new(FlushStrategy::End);
        assert!(flush_control.should_flush_end());
    }

    #[tokio::test]
    async fn get_flush_interval() {
        let flush_control = FlushControl::new(FlushStrategy::Default);
        assert_eq!(
            flush_control.get_flush_interval().period().as_millis(),
            DEFAULT_FLUSH_INTERVAL as u128
        );

        let flush_control = FlushControl::new(FlushStrategy::Periodically(PeriodicStrategy {
            interval: 1,
        }));
        assert_eq!(flush_control.get_flush_interval().period().as_millis(), 1);

        let flush_control = FlushControl::new(FlushStrategy::End);
        assert_eq!(
            flush_control.get_flush_interval().period().as_millis(),
            tokio::time::Duration::MAX.as_millis()
        );
    }
}
