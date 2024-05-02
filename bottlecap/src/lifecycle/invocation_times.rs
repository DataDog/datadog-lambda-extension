const LOOKBACK_COUNT: usize = 20;
const ONE_TWENTY_SECONDS: f64 = 120.0;

pub struct InvocationTimes {
    times: [u64; LOOKBACK_COUNT],
    head: usize,
}

impl InvocationTimes {
    pub fn new() -> InvocationTimes {
        InvocationTimes { times: [0; LOOKBACK_COUNT], head: 0 }
    }

    pub fn add(&mut self, timestamp: u64) {
        self.times[self.head] = timestamp;
        self.head = (self.head + 1) % LOOKBACK_COUNT;
    }
    
    pub fn should_adapt_to_periodic(&self, now: u64) -> bool {
        let mut count = 0;
        let mut last = 0;
        for time in self.times.iter() {
            if *time != 0 {
                count += 1;
                last = *time;
            }
        }
        // If we haven't seen enough invocations, we should flush
        if count < LOOKBACK_COUNT {
            return false;
        }
        let elapsed = now - last;
        (elapsed as f64 / (count - 1) as f64) < ONE_TWENTY_SECONDS
    }
}

#[cfg(test)]
mod tests {
    use crate::lifecycle::invocation_times;

    #[test]
    fn new() {
        let invocation_times = invocation_times::InvocationTimes::new();
        assert_eq!(invocation_times.times, [0; invocation_times::LOOKBACK_COUNT]);
        assert_eq!(invocation_times.head, 0);
    }

    #[test]
    fn insertion() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        let timestamp = 1;
        invocation_times.add(timestamp);
        assert_eq!(invocation_times.times[0], timestamp);
        assert_eq!(invocation_times.head, 1);
        assert_eq!(invocation_times.should_adapt_to_periodic(1), false);
    }
    
    #[test]
    fn insertion_with_full_buffer_fast_invokes() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        for i in 0..invocation_times::LOOKBACK_COUNT+1 {
            invocation_times.add(i as u64);
        }
        // should wrap around
        assert_eq!(invocation_times.times[0], 20);
        assert_eq!(invocation_times.head, 1);
        assert_eq!(invocation_times.should_adapt_to_periodic(21), true);
    }

    #[test]
    fn insertion_with_full_buffer_slow_invokes() {
        let mut invocation_times = invocation_times::InvocationTimes::new();
        invocation_times.add(1 as u64);
        for i in 0..invocation_times::LOOKBACK_COUNT {
            invocation_times.add((i + 5000) as u64);
        }
        // should wrap around
        assert_eq!(invocation_times.times[0], 5019);
        assert_eq!(invocation_times.head, 1);
        assert_eq!(invocation_times.should_adapt_to_periodic(10000), false);
    }
}
