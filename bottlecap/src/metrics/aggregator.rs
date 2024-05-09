//! The aggregation of metrics.

use crate::metrics::metric;
use crate::metrics::{
    constants, datadog, errors,
    metric::{Metric, Type},
};

use datadog_protos::metrics::{Dogsketch, Sketch, SketchPayload};
use hashbrown::hash_table;
use std::time;
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

#[derive(Debug, Clone)]
struct Entry {
    id: u64,
    name: Ustr,
    tags: Option<Ustr>,
    kind: metric::Type,
    metric_value: MetricValue,
}

#[derive(Debug, Clone)]
struct DistributionMetric {
    pub sketch: ddsketch_agent::DDSketch,
}

#[derive(Debug, Clone)]
struct CountMetric {
    value: f64,
}

#[derive(Debug, Clone)]
struct GaugeMetric {
    value: f64,
}

#[derive(Debug, Clone)]
enum MetricValue {
    Count(CountMetric),
    Gauge(GaugeMetric),
    Distribution(DistributionMetric),
}

trait InsertMetric {
    fn insert_metric(&mut self, metric: &Metric);
}

trait GetValue {
    fn get_value(&self) -> f64;
}

trait GetSketch {
    fn get_sketch(&self) -> &ddsketch_agent::DDSketch;
}

impl InsertMetric for MetricValue {
    fn insert_metric(&mut self, metric: &Metric) {
        match self {
            MetricValue::Count(count_metric) => {
                count_metric.insert_metric(metric);
            }
            MetricValue::Gauge(gauge_metric) => {
                gauge_metric.insert_metric(metric);
            }
            MetricValue::Distribution(distribution_metric) => {
                distribution_metric.insert_metric(metric);
            }
        }
    }
}

impl GetSketch for MetricValue {
    fn get_sketch(&self) -> &ddsketch_agent::DDSketch {
        match self {
            MetricValue::Count(_) => unreachable!(),
            MetricValue::Gauge(_) => unreachable!(),
            MetricValue::Distribution(distribution_metric) => &distribution_metric.sketch,
        }
    }
}

impl GetValue for MetricValue {
    fn get_value(&self) -> f64 {
        match self {
            MetricValue::Count(count_metric) => count_metric.value,
            MetricValue::Gauge(gauge_metric) => gauge_metric.value,
            MetricValue::Distribution(_distribution_metric) => unreachable!(),
        }
    }
}

impl InsertMetric for DistributionMetric {
    fn insert_metric(&mut self, metric: &Metric) {
        // safe because we know there's at least one value when we parse
        self.sketch.insert(metric.first_value().unwrap_or_default());
    }
}

impl InsertMetric for GaugeMetric {
    fn insert_metric(&mut self, metric: &Metric) {
        // safe because we know there's at least one value when we parse
        self.value = metric.first_value().unwrap_or_default();
    }
}

impl InsertMetric for CountMetric {
    fn insert_metric(&mut self, metric: &Metric) {
        // safe because we know there's at least one value when we parse
        self.value += metric.first_value().unwrap_or_default();
    }
}

impl Entry {
    fn new_from_metric(id: u64, metric: &Metric) -> Self {
        let new_metric_value = match metric.first_value() {
            Ok(value) => value,
            Err(e) => {
                error!("failed to parse metric: {:?}", e);
                0.0
            }
        };
        Self {
            id,
            metric_value: match metric.kind {
                Type::Count => MetricValue::Count(CountMetric {
                    value: new_metric_value,
                }),
                Type::Gauge => MetricValue::Gauge(GaugeMetric {
                    value: new_metric_value,
                }),
                Type::Distribution => {
                    let mut empty_sketch = MetricValue::Distribution(DistributionMetric {
                        sketch: ddsketch_agent::DDSketch::default(),
                    });
                    empty_sketch.insert_metric(metric);
                    empty_sketch
                }
            },
            name: metric.name,
            tags: metric.tags,
            kind: metric.kind,
        }
    }

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

#[derive(Clone)]
// NOTE by construction we know that intervals and contexts do not explore the
// full space of usize but the type system limits how we can express this today.
pub struct Aggregator<const CONTEXTS: usize> {
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
            map: hash_table::HashTable::new(),
        })
    }

    /// Insert a `Metric` into the `Aggregator` at the current interval
    ///
    /// # Errors
    ///
    /// Function will return overflow error if more than
    /// `min(constants::MAX_CONTEXTS, CONTEXTS)` is exceeded.
    pub fn insert(&mut self, metric: &Metric) -> Result<(), errors::Insert> {
        let id = metric::id(metric.name, metric.tags);
        let len = self.map.len();

        match self.map.entry(
            id,
            |m| m.id == id,
            |m| crate::metrics::metric::id(m.name, m.tags),
        ) {
            hash_table::Entry::Vacant(entry) => {
                if len >= CONTEXTS {
                    return Err(errors::Insert::Overflow);
                }
                let ent = Entry::new_from_metric(id, metric);
                entry.insert(ent);
            }
            hash_table::Entry::Occupied(mut entry) => {
                entry.get_mut().metric_value.insert_metric(metric);
            }
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn distributions_to_protobuf(&self) -> SketchPayload {
        let mut sketch_payload = SketchPayload::new();
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        for entry in &self.map {
            if entry.kind != metric::Type::Distribution {
                continue;
            }
            let sketch = entry.metric_value.get_sketch();
            let mut dogsketch = Dogsketch::default();
            sketch.merge_to_dogsketch(&mut dogsketch);
            // TODO(Astuyve) allow users to specify timestamp
            dogsketch.set_ts(now);
            let mut sketch = Sketch::default();
            sketch.set_dogsketches(vec![dogsketch]);
            let tags = entry.tags.unwrap_or_default().to_string();
            let name = entry.name.to_string();
            sketch.set_metric(name.clone().into());
            sketch.set_tags(vec![tags.clone().into()]);
            sketch_payload.sketches.push(sketch);
        }
        sketch_payload
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn to_series(&self) -> datadog::Series {
        // TODO it would be really slick to use a bump allocator here since
        // there's so many tiny allocations
        let mut series = datadog::Series {
            series: Vec::with_capacity(1_024),
        };
        for entry in &self.map {
            if entry.kind == metric::Type::Distribution {
                continue;
            }
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
                metric::Type::Distribution => unreachable!(),
            };
            let point = datadog::Point {
                value: entry.metric_value.get_value(),
                // TODO(astuyve) allow user to specify timestamp
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
                final_tags = tags.split(',').map(|tag| tag.to_string()).collect();
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
        metric::{self, Metric},
        Aggregator,
    };

    #[test]
    fn insertion() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        // Both unique contexts get one slot.
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    fn distribution_insertion() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|d|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|d|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        // Both unique contexts get one slot.
        assert_eq!(aggregator.map.len(), 2);
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
        assert_eq!(aggregator.map.len(), 1);

        assert!(aggregator.insert(&metric2).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());
        assert_eq!(aggregator.map.len(), 2);

        assert!(aggregator.insert(&metric3).is_err());
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    fn clear() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        aggregator.clear();
        assert_eq!(aggregator.map.len(), 0);
    }

    #[test]
    fn to_series() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");
        let metric3 = Metric::parse("bar:1|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.to_series().len(), 2);
        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.to_series().len(), 2);
        assert_eq!(aggregator.map.len(), 2);

        assert!(aggregator.insert(&metric3).is_err());
        assert_eq!(aggregator.to_series().len(), 2);
    }

    #[test]
    fn distributions_to_protobuf() {
        let mut aggregator = Aggregator::<2>::new().unwrap();

        let metric1 = Metric::parse("test:1|d|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|d|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 2);
        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 2);
        assert_eq!(aggregator.map.len(), 2);
    }
}
