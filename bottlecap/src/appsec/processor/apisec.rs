use std::hash::{BuildHasher, BuildHasherDefault, Hasher};
use std::num::NonZero;
use std::time::Duration;

#[cfg(test)]
use mock_instant::global::Instant;
#[cfg(not(test))]
use std::time::Instant;

use fnv::FnvBuildHasher;
use ordered_hash_map::OrderedHashMap;

/// A sampler that implements time-based, per-endpoint sampling.
///
/// It is an implementation of <https://docs.google.com/document/d/1PYoHms9PPXR8V_5_T5-KXAhoFDKQYA8mTnmS12xkGOE/edit?pli=1&tab=t.0#heading=h.rnd972k0hiye>
/// that does not have adaptations for thread-based concurrency, as the
/// Serverless extension does not actually present significant risk of
/// contention.
#[derive(Debug)]
pub(crate) struct Sampler {
    interval: Duration,
    data: OrderedHashMap<u64, SamplerState, BuildIdentityHasher>,
    hasher_builder: FnvBuildHasher,
}
impl Sampler {
    /// Creates a new sampler with the given interval and a default capacity.
    pub(crate) fn with_interval(interval: Duration) -> Self {
        Self::with_interval_and_capacity(interval, unsafe { NonZero::new_unchecked(4_096) })
    }

    /// Creates a new sampler with the given interval and capacity.
    pub(crate) fn with_interval_and_capacity(interval: Duration, capacity: NonZero<usize>) -> Self {
        Self {
            interval,
            data: OrderedHashMap::with_capacity_and_hasher(
                capacity.get(),
                BuildIdentityHasher::default(),
            ),
            hasher_builder: FnvBuildHasher::default(),
        }
    }

    /// Make a sampling decision for a given request signature.
    pub(crate) fn decision_for(&mut self, method: &str, route: &str, status_code: &str) -> bool {
        let mut hasher = self.hasher_builder.build_hasher();
        hasher.write(method.as_bytes());
        hasher.write_u8(0);
        hasher.write(route.as_bytes());
        hasher.write_u8(0);
        hasher.write(status_code.as_bytes());
        let hash = hasher.finish();

        if let Some(state) = self.data.get_mut(&hash) {
            if state.last_sample.elapsed() >= self.interval {
                state.last_sample = Instant::now();
                // This entry is now the most recent one!
                self.data.move_to_back(&hash);
                true
            } else {
                // This entry is now the most recent one!
                self.data.move_to_back(&hash);
                false
            }
        } else {
            if self.data.len() >= self.data.capacity() {
                // We're full, drop the oldest entry...
                self.data.pop_front();
            }
            self.data.insert(
                hash,
                SamplerState {
                    last_sample: Instant::now(),
                },
            );
            true
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
struct SamplerState {
    last_sample: Instant,
}

type BuildIdentityHasher = BuildHasherDefault<IdentityHasher>;

/// A [Hasher] that forwards the value that is being hashed without any additonal processing.
#[derive(Debug, Default, Clone, Copy)]
struct IdentityHasher(u64);
#[allow(clippy::cast_sign_loss)] // This is not relevant in this Hasher implementation
impl Hasher for IdentityHasher {
    #[cfg_attr(coverage_nightly, coverage(off))] // Unsupported
    fn write(&mut self, _: &[u8]) {
        unimplemented!("IdentityHasher does not support hashing arbitrary data")
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_u8(&mut self, v: u8) {
        self.0 = u64::from(v);
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_u16(&mut self, v: u16) {
        self.0 = u64::from(v);
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_u32(&mut self, v: u32) {
        self.0 = u64::from(v);
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_usize(&mut self, v: usize) {
        self.0 = v as u64;
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_i8(&mut self, v: i8) {
        self.0 = v as u64;
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_i16(&mut self, v: i16) {
        self.0 = v as u64;
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_i32(&mut self, v: i32) {
        self.0 = v as u64;
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_i64(&mut self, v: i64) {
        self.0 = v as u64;
    }
    #[cfg_attr(coverage_nightly, coverage(off))] // Supported, but unused
    fn write_isize(&mut self, v: isize) {
        self.0 = v as u64;
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg_attr(coverage_nightly, coverage(off))] // Test modules skew coverage metrics
#[cfg(test)]
mod tests {
    use mock_instant::global::MockClock;

    use super::*;

    #[test]
    fn test_default_sampler() {
        let mut sampler = Sampler::with_interval(Duration::from_secs(30));
        // First call should be sampled
        assert!(sampler.decision_for("GET", "/", "200"));

        MockClock::advance(Duration::from_secs(15));
        // Second call should not be sampled (less than 30 seconds have passed)
        assert!(!sampler.decision_for("GET", "/", "200"));

        MockClock::advance(Duration::from_secs(15));
        // 30 seconds have passed, the call should be sampled in again!
        assert!(sampler.decision_for("GET", "/", "200"));
    }

    #[test]
    fn test_sampler_capacity() {
        let mut sampler = Sampler::with_interval_and_capacity(Duration::from_secs(30), unsafe {
            NonZero::new_unchecked(1)
        });

        for i in 0..100 {
            // All requests should be sampled in here since we run with a capacity of 1 and use a differen route each time...
            assert!(sampler.decision_for("GET", &format!("/{i}"), "200"));
        }
    }
}
