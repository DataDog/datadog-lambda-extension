// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashSet, VecDeque};

const DEFAULT_CAPACITY: usize = 100;

/// A key for span deduplication consisting of `trace_id` and `span_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DedupKey {
    pub trace_id: u64,
    pub span_id: u64,
}

impl DedupKey {
    /// Creates a new `SpanDedupKey`.
    #[must_use]
    pub fn new(trace_id: u64, span_id: u64) -> Self {
        Self { trace_id, span_id }
    }
}

/// A deduplicator that maintains a fixed-size cache of recently seen span keys.
/// When the capacity is reached, the oldest key is evicted (FIFO).
pub struct Deduper {
    /// Set for O(1) existence checks
    seen: HashSet<DedupKey>,
    /// Queue to maintain insertion order for FIFO eviction
    order: VecDeque<DedupKey>,
    /// Maximum number of keys to keep
    capacity: usize,
}

impl Default for Deduper {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl Deduper {
    /// Creates a new `Deduper` with the specified capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of IDs to track
    ///
    /// # Examples
    ///
    /// ```
    /// use bottlecap::traces::span_dedup::Deduper;
    ///
    /// let deduper = Deduper::new(100);
    /// ```
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            seen: HashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Checks if a span key exists and adds it if it doesn't.
    ///
    /// # Arguments
    ///
    /// * `key` - The span key to check and potentially add
    ///
    /// # Returns
    ///
    /// `true` if the key was added (didn't exist before), `false` if it already existed
    ///
    /// # Examples
    ///
    /// ```
    /// use bottlecap::traces::span_dedup::{Deduper, DedupKey};
    ///
    /// let mut deduper = Deduper::default();
    /// let key = DedupKey::new(123, 456);
    /// assert!(deduper.check_and_add(key)); // Returns true, key was added
    /// assert!(!deduper.check_and_add(key)); // Returns false, key already exists
    /// ```
    pub fn check_and_add(&mut self, key: DedupKey) -> bool {
        // If the key already exists, return false
        if self.seen.contains(&key) {
            return false;
        }

        // If we're at capacity, evict the oldest entry
        if self.order.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.seen.remove(&oldest);
        }

        // Add the new key
        self.seen.insert(key);
        self.order.push_back(key);

        // Return true to indicate the key was added
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_deduper() {
        let deduper = Deduper::new(10);
        assert_eq!(deduper.seen.len(), 0);
    }

    #[test]
    fn test_default_deduper() {
        let deduper = Deduper::default();
        assert_eq!(deduper.capacity, DEFAULT_CAPACITY);
    }

    #[test]
    fn test_check_and_add() {
        let mut deduper = Deduper::new(3);

        let key1 = DedupKey::new(100, 123);
        let key2 = DedupKey::new(100, 456);

        // First call should return true (key was added)
        assert!(deduper.check_and_add(key1));

        // Second call should return false (key already exists)
        assert!(!deduper.check_and_add(key1));

        // Different key should return true
        assert!(deduper.check_and_add(key2));

        // Calling again should return false
        assert!(!deduper.check_and_add(key2));
    }

    #[test]
    fn test_check_and_add_with_eviction() {
        let mut deduper = Deduper::new(2);

        let key1 = DedupKey::new(1, 10);
        let key2 = DedupKey::new(2, 20);
        let key3 = DedupKey::new(3, 30);

        assert!(deduper.check_and_add(key1));
        assert!(deduper.check_and_add(key2));

        // Adding 3rd should evict key1
        assert!(deduper.check_and_add(key3));

        // Now key1 should be addable again (was evicted)
        assert!(deduper.check_and_add(key1));

        // But key2 should have been evicted
        assert!(deduper.check_and_add(key2));
    }

    #[test]
    fn test_same_trace_id_different_span_id() {
        let mut deduper = Deduper::new(3);

        let key1 = DedupKey::new(100, 1);
        let key2 = DedupKey::new(100, 2);

        // Both should be added even though they have the same trace_id
        assert!(deduper.check_and_add(key1));
        assert!(deduper.check_and_add(key2));
    }

    #[test]
    fn test_same_span_id_different_trace_id() {
        let mut deduper = Deduper::new(3);

        let key1 = DedupKey::new(1, 100);
        let key2 = DedupKey::new(2, 100);

        // Both should be added even though they have the same span_id
        assert!(deduper.check_and_add(key1));
        assert!(deduper.check_and_add(key2));
    }
}
