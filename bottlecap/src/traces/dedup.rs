// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashSet, VecDeque};

const DEFAULT_MAX_SIZE: usize = 50;

/// A deduplicator that maintains a fixed-size cache of recently seen IDs.
/// When the maximum size is reached, the oldest ID is evicted (FIFO).
pub struct Deduper {
    /// Set for O(1) existence checks
    seen: HashSet<u64>,
    /// Queue to maintain insertion order for FIFO eviction
    order: VecDeque<u64>,
    /// Maximum number of IDs to keep
    max_size: usize,
}

impl Deduper {
    /// Creates a new `Deduper` with the specified maximum size.
    ///
    /// # Arguments
    ///
    /// * `max_size` - Maximum number of IDs to track
    ///
    /// # Examples
    ///
    /// ```
    /// use bottlecap::traces::dedup::Deduper;
    ///
    /// let deduper = Deduper::new(100);
    /// ```
    #[must_use]
    pub fn new(max_size: usize) -> Self {
        Self {
            seen: HashSet::with_capacity(max_size),
            order: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    /// Creates a new `Deduper` with the default maximum size of 50.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottlecap::traces::dedup::Deduper;
    ///
    /// let deduper = Deduper::default();
    /// ```
    #[must_use]
    pub fn default() -> Self {
        Self::new(DEFAULT_MAX_SIZE)
    }

    /// Checks if an ID exists and adds it if it doesn't.
    ///
    /// # Arguments
    ///
    /// * `id` - The ID to check and potentially add
    ///
    /// # Returns
    ///
    /// `true` if the ID was added (didn't exist before), `false` if it already existed
    ///
    /// # Examples
    ///
    /// ```
    /// use bottlecap::traces::dedup::Deduper;
    ///
    /// let mut deduper = Deduper::default();
    /// assert!(deduper.check_and_add(12345)); // Returns true, ID was added
    /// assert!(!deduper.check_and_add(12345)); // Returns false, ID already exists
    /// ```
    pub fn check_and_add(&mut self, id: u64) -> bool {
        // If the ID already exists, return false
        if self.seen.contains(&id) {
            return false;
        }

        // If we're at max capacity, evict the oldest entry
        if self.order.len() >= self.max_size {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }

        // Add the new ID
        self.seen.insert(id);
        self.order.push_back(id);
        
        // Return true to indicate the ID was added
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
        assert_eq!(deduper.max_size, DEFAULT_MAX_SIZE);
    }

    #[test]
    fn test_check_and_add() {
        let mut deduper = Deduper::new(3);
        
        // First call should return true (ID was added)
        assert!(deduper.check_and_add(123));
        
        // Second call should return false (ID already exists)
        assert!(!deduper.check_and_add(123));
        
        // Different ID should return true
        assert!(deduper.check_and_add(456));
        
        // Calling again should return false
        assert!(!deduper.check_and_add(456));
    }

    #[test]
    fn test_check_and_add_with_eviction() {
        let mut deduper = Deduper::new(2);
        
        assert!(deduper.check_and_add(1));
        assert!(deduper.check_and_add(2));
        
        // Adding 3rd should evict 1
        assert!(deduper.check_and_add(3));
        
        // Now 1 should be addable again (was evicted)
        assert!(deduper.check_and_add(1));
        
        // But 2 should have been evicted
        assert!(deduper.check_and_add(2));
    }
}

