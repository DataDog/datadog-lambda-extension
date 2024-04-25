//! The aggregation of metrics.

use crate::metrics::metric;
use crate::metrics::{constants, errors, datadog, metric::{Metric, Type}};

use std::time;

use hashbrown::hash_table;
use tracing::error;
use ustr::Ustr;

/// Error for the `aggregate` function
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Metric insertion failed, possibly with recovery
    #[error(transparent)]
    Insert(#[from] errors::Insert),
    /// Creation failed, unable to recover
    #[error(transparent)]
    Creation(#[from] errors::Creation),
    /// IO error
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    id: u64,
    generation: u16,
    name: Ustr,
    tags: Option<Ustr>,
    kind: metric::Type,
    value: f64,
}

impl Entry {
    /// Return an iterator over key, value pairs
    fn tag(&self) -> impl Iterator<Item = (Ustr, Ustr)> {
        self.tags.into_iter().filter_map(|tags| {
            let mut split = tags.split(',');
            match (split.next(), split.next()) {
                (Some(k), Some(v)) => Some((Ustr::from(k), Ustr::from(v))),
                _ => None, // Skip tags that lack the proper format
            }
        })
    }
}

impl Entry {
    fn of_generation(generation: u16, id: u64, metric: &Metric) -> Self {
        Self {
            id,
            generation,
            value: metric.values().next().unwrap().unwrap(),
            name: metric.name,
            tags: metric.tags,
            kind: metric.kind,
        }
    }

    #[inline]
    fn aged_out(self, generation: u16) -> bool {
        self.generation.abs_diff(generation) >= 2
    }
}

#[derive(Clone)]
// NOTE by construction we know that intervals and contexts do not explore the
// full space of usize but the type system limits how we can express this today.
pub struct Aggregator<const CONTEXTS: usize> {
    generation: u16,
    map: hash_table::HashTable<Entry>,
}

impl<const CONTEXTS: usize> Aggregator<CONTEXTS> {
    /// Create a new instance of `Aggregator`
    ///
    /// # Errors
    ///
    /// Will fail at runtime if the type `INTERVALS` and `CONTEXTS` exceed their
    /// counterparts in `constants`. This would be better as a compile-time
    /// issue, although leaving this open allows for runtime configuration.
    #[allow(clippy::cast_precision_loss)]
    pub fn new() -> Result<Self, errors::Creation> {
        if CONTEXTS > constants::MAX_CONTEXTS {
            return Err(errors::Creation::Contexts);
        }

        Ok(Self {
            generation: 0,
            map: hash_table::HashTable::new(),
        })
    }

    /// Remove an members of the Aggregator that are outside the generation
    /// window. (Arbitrarily, 2 generations.) The length of the Aggregator after
    /// calling this function will be reduced by the value returned.
    ///
    /// Returns the number of purged entries.
    fn purge(&mut self) -> usize {
        let total_removed = self
            .map
            .extract_if(|entry| entry.aged_out(self.generation))
            .fold(0, |acc, _| acc + 1);
        self.generation = self.generation.wrapping_add(1);
        total_removed
    }

    /// Scan the

    /// Insert a `Metric` into the `Aggregator` at the current interval
    ///
    /// # Errors
    ///
    /// Function will return overflow error if more than
    /// `min(constants::MAX_CONTEXTS, CONTEXTS)` is exceeded.
    pub fn insert(&mut self, metric: &Metric) -> Result<(), errors::Insert> {
        let id = metric::id(metric.name, metric.tags);
        let len = self.map.len();

        match self
            .map
            .entry(id, |m| m.id == id, |m| crate::metrics::metric::id(m.name, m.tags))
        {
            hash_table::Entry::Vacant(entry) => {
                if len >= CONTEXTS {
                    return Err(errors::Insert::Overflow);
                }
                let ent = Entry::of_generation(self.generation, id, metric);
                entry.insert(ent);
            }
            hash_table::Entry::Occupied(mut entry) => match metric.kind {
                Type::Count => {
                    for value in metric.values() {
                        entry.get_mut().value += value?;
                    }
                }
                Type::Gauge => {
                    for value in metric.values() {
                        entry.get_mut().value = value?;
                    }
                }
                Type::Distribution => {
                    // Todo - grab Toby's implementation of ddsketch
                }
            },
        }

        Ok(())
    }

    /// Return true if the instance is empty -- has no metrics -- else false.
    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Return the number of elements currently maintained in the table.
    fn len(&self) -> usize {
        self.map.len()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn to_series(&self) -> datadog::Series {
        // TODO it would be really slick to use a bump allocator here since
        // there's so many tiny allocations
        let mut series = datadog::Series {
            series: Vec::with_capacity(1_024),
        };
        for entry in &self.map {
            let mut resources = Vec::with_capacity(constants::MAX_TAGS);
            for (name, kind) in entry.tag() {
                let resource = datadog::Resource {
                    name: name.as_str(),
                    kind: kind.as_str(),
                };
                resources.push(resource);
            }
            let kind = match entry.kind {
                metric::Type::Count => datadog::DdMetricKind::Count,
                metric::Type::Gauge => datadog::DdMetricKind::Gauge,
                metric::Type::Distribution => datadog::DdMetricKind::Distribution,
            };
            let point = datadog::Point {
                value: entry.value,
                timestamp: time::SystemTime::now()
                    .duration_since(time::UNIX_EPOCH)
                    .expect("unable to poll clock, unrecoverable")
                    .as_secs(),
            };

            let mut final_tags = Vec::new();
            // TODO
            // These tags are interned so we don't need to clone them here but we're just doing it
            // because it's easier than dealing with the lifetimes.
            if let Some(tags) = entry.tags {
                final_tags = tags
                    .split(',')
                    .map(|tag| tag.to_string())
                    .collect();
            }
            let metric = datadog::Metric {
                metric: entry.name.as_str(),
                resources,
                kind,
                points: [point; 1],
                tags: final_tags,
            };
            series.series.push(metric);
        }
        series
    }
}

#[cfg(test)]
mod tests {
    use crate::metrics::aggregator::{
        Aggregator,
        metric::{self, Metric},
    };

    #[test]
    fn insertion() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        // Both unique contexts get one slot.
        assert_eq!(aggregator.len(), 2);
    }

    #[test]
    fn overflow() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");
        let metric3 = Metric::parse("bar:1|c|k:v").expect("metric parse failed");

        let id1 = metric::id(metric1.name, metric1.tags);
        let id2 = metric::id(metric2.name, metric2.tags);
        let id3 = metric::id(metric3.name, metric3.tags);

        assert!(id1 != id2);
        assert!(id1 != id3);
        assert!(id2 != id3);

        assert!(aggregator.insert(&metric1).is_ok());
        assert_eq!(aggregator.len(), 1);

        assert!(aggregator.insert(&metric2).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());
        assert_eq!(aggregator.len(), 2);

        assert!(aggregator.insert(&metric3).is_err());
        assert_eq!(aggregator.len(), 2);
    }

    #[test]
    fn purge() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        // No metrics age out after the first purge
        assert_eq!(aggregator.generation, 0);
        assert_eq!(aggregator.purge(), 0);
        assert_eq!(aggregator.len(), 2);
        // No metrics age out after the second purge
        assert_eq!(aggregator.generation, 1);
        assert_eq!(aggregator.purge(), 0);
        assert_eq!(aggregator.len(), 2);
        // Two metrics age out after the last purge
        assert_eq!(aggregator.generation, 2);
        assert_eq!(aggregator.purge(), 2);
        assert_eq!(aggregator.len(), 0);
        // No metrics exist
        assert_eq!(aggregator.generation, 3);
        assert_eq!(aggregator.purge(), 0);
        assert_eq!(aggregator.len(), 0);
    }

    #[test]
    fn to_series() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");
        let metric3 = Metric::parse("bar:1|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        assert_eq!(aggregator.len(), 2);
        assert_eq!(aggregator.to_series().len(), 2);
        assert_eq!(aggregator.len(), 2);
        assert_eq!(aggregator.to_series().len(), 2);
        assert_eq!(aggregator.len(), 2);

        assert!(aggregator.insert(&metric3).is_err());
        assert_eq!(aggregator.to_series().len(), 2);
    }
}
