//! The aggregation of metrics.

use crate::{
    metrics::{constants, datadog, errors, metric},
    tags::provider,
};
use datadog::Metric as DatadogMetric;
use metric::{Metric, Type};
use std::sync::Arc;

use datadog_protos::metrics::{Dogsketch, Sketch, SketchPayload};
use ddsketch_agent::DDSketch;
use hashbrown::hash_table;
use protobuf::Message;
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
    metric_value: MetricValue,
}

#[derive(Debug, Clone)]
enum MetricValue {
    Count(f64),
    Gauge(f64),
    Distribution(DDSketch),
}

impl MetricValue {
    fn insert_metric(&mut self, metric: &Metric) {
        // safe because we know there's at least one value when we parse
        match self {
            MetricValue::Count(count) => *count += metric.first_value().unwrap_or_default(),
            MetricValue::Gauge(gauge) => *gauge = metric.first_value().unwrap_or_default(),
            MetricValue::Distribution(distribution) => {
                distribution.insert(metric.first_value().unwrap_or_default());
            }
        }
    }

    fn get_value(&self) -> Option<f64> {
        match self {
            MetricValue::Count(count) => Some(*count),
            MetricValue::Gauge(gauge) => Some(*gauge),
            MetricValue::Distribution(_) => None,
        }
    }

    fn get_sketch(&self) -> Option<&DDSketch> {
        match self {
            MetricValue::Distribution(distribution) => Some(distribution),
            _ => None,
        }
    }
}

impl Entry {
    fn new_from_metric(id: u64, metric: &Metric) -> Self {
        let mut metric_value = match metric.kind {
            Type::Count => MetricValue::Count(0.0),
            Type::Gauge => MetricValue::Gauge(0.0),
            Type::Distribution => MetricValue::Distribution(DDSketch::default()),
        };
        metric_value.insert_metric(metric);
        Self {
            id,
            name: metric.name,
            tags: metric.tags,
            metric_value,
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

#[derive(Debug, Clone)]
enum Limit {
    ApiLimit(usize, usize),
}

#[derive(Clone)]
// NOTE by construction we know that intervals and contexts do not explore the
// full space of usize but the type system limits how we can express this today.
pub struct Aggregator<const CONTEXTS: usize> {
    tags_provider: Arc<provider::Provider>,
    map: hash_table::HashTable<Entry>,
    no_limit: Limit,
    series_limit: Limit,
    distribution_limit: Limit,
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
    pub fn new(tags_provider: Arc<provider::Provider>) -> Result<Self, errors::Creation> {
        if CONTEXTS > constants::MAX_CONTEXTS {
            return Err(errors::Creation::Contexts);
        }
        Ok(Self {
            tags_provider,
            map: hash_table::HashTable::new(),
            no_limit: Limit::ApiLimit(usize::MAX, usize::MAX),
            series_limit: Limit::ApiLimit(
                constants::MAX_ENTRIES_SINGLE_METRIC,
                constants::MAX_SIZE_BYTES_SINGLE_METRIC,
            ),
            distribution_limit: Limit::ApiLimit(
                constants::MAX_ENTRIES_SKETCH_METRIC,
                constants::MAX_SIZE_SKETCH_METRIC,
            ),
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

        match self
            .map
            .entry(id, |m| m.id == id, |m| metric::id(m.name, m.tags))
        {
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

    #[must_use]
    pub fn distributions_to_protobuf(&self) -> SketchPayload {
        self.distributions_to_protobuf_limited(&self.no_limit)
    }

    #[must_use]
    pub fn distributions_to_protobuf_api_limited(&self) -> SketchPayload {
        self.distributions_to_protobuf_limited(&self.distribution_limit)
    }

    #[must_use]
    fn distributions_to_protobuf_limited(&self, limit: &Limit) -> SketchPayload {
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let mut sketch_payload = SketchPayload::new();
        let (entry_limit, size_limit) = match *limit {
            Limit::ApiLimit(entry_limit, size_limit) => (entry_limit, size_limit),
        };
        for entry in &self.map {
            let Some(sketch) = self.build_sketch(now, entry) else {
                continue;
            };
            if sketch_payload.compute_size() + sketch.compute_size() > size_limit as u64 {
                break;
            }

            sketch_payload.sketches.push(sketch);

            if sketch_payload.sketches.len() >= entry_limit {
                break;
            }
        }
        sketch_payload
    }

    fn build_sketch(&self, now: i64, entry: &Entry) -> Option<Sketch> {
        if let MetricValue::Distribution(_) = entry.metric_value {
        } else {
            return None;
        };
        let sketch = entry.metric_value.get_sketch()?;
        let mut dogsketch = Dogsketch::default();
        sketch.merge_to_dogsketch(&mut dogsketch);
        // TODO(Astuyve) allow users to specify timestamp
        dogsketch.set_ts(now);
        let mut sketch = Sketch::default();
        sketch.set_dogsketches(vec![dogsketch]);
        let name = entry.name.to_string();
        sketch.set_metric(name.clone().into());
        let mut base_tag_vec = self.tags_provider.get_tags_vec();
        let mut tags = tags_string_to_vector(entry.tags);
        base_tag_vec.append(&mut tags); // TODO split on comma
        sketch.set_tags(
            base_tag_vec
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
        );
        Some(sketch)
    }

    fn build_metric(&self, entry: &Entry) -> Option<DatadogMetric> {
        if let MetricValue::Distribution(_) = entry.metric_value {
            return None;
        };
        let mut resources = Vec::with_capacity(constants::MAX_TAGS);
        for (name, kind) in entry.tag() {
            let resource = datadog::Resource {
                name: name.as_str(),
                kind: kind.as_str(),
            };
            resources.push(resource);
        }
        let kind = match entry.metric_value {
            MetricValue::Count(_) => datadog::DdMetricKind::Count,
            MetricValue::Gauge(_) => datadog::DdMetricKind::Gauge,
            MetricValue::Distribution(_) => unreachable!(),
        };
        let point = datadog::Point {
            value: entry.metric_value.get_value()?,
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
            final_tags = tags
                .split(',')
                .map(std::string::ToString::to_string)
                .collect();
        }
        final_tags.append(&mut self.tags_provider.get_tags_vec());
        Some(DatadogMetric {
            metric: entry.name.as_str(),
            resources,
            kind,
            points: [point; 1],
            tags: final_tags,
        })
    }

    #[must_use]
    pub fn to_series(&self) -> datadog::Series {
        self.to_series_limited(&self.no_limit)
    }

    #[must_use]
    pub fn to_series_api_limited(&self) -> datadog::Series {
        self.to_series_limited(&self.series_limit)
    }

    #[must_use]
    fn to_series_limited(&self, limit: &Limit) -> datadog::Series {
        let mut series = datadog::Series {
            series: Vec::with_capacity(1_024),
        };
        let (entry_limit, size_limit) = match *limit {
            Limit::ApiLimit(entry_limit, size_limit) => (entry_limit, size_limit),
        };
        for entry in &self.map {
            let Some(metric) = self.build_metric(entry) else {
                continue;
            };

            let serialized_metric = match serde_json::to_vec(&metric) {
                Ok(serialized_metric) => serialized_metric,
                Err(e) => {
                    error!("failed to serialize metric: {:?}", e);
                    continue;
                }
            };
            if serialized_metric.len() + series.series.len() > size_limit {
                break;
            }
            series.series.push(metric);

            if series.series.len() >= entry_limit {
                break;
            }
        }
        series
    }

    #[cfg(test)]
    pub fn get_sketch_by_id(&mut self, name: Ustr, tags: Option<Ustr>) -> Option<DDSketch> {
        let id = metric::id(name, tags);

        match self
            .map
            .entry(id, |m| m.id == id, |m| metric::id(m.name, m.tags))
        {
            hash_table::Entry::Vacant(_) => None,
            hash_table::Entry::Occupied(entry) => entry.get().metric_value.get_sketch().cloned(),
        }
    }

    #[cfg(test)]
    pub fn get_value_by_id(&mut self, name: Ustr, tags: Option<Ustr>) -> Option<f64> {
        let id = metric::id(name, tags);

        match self.map.entry(
            id,
            |m| m.id == id,
            |m| crate::metrics::metric::id(m.name, m.tags),
        ) {
            hash_table::Entry::Vacant(_) => None,
            hash_table::Entry::Occupied(entry) => entry.get().metric_value.get_value(),
        }
    }
}

fn tags_string_to_vector(tags: Option<Ustr>) -> Vec<String> {
    if tags.is_none() {
        return Vec::new();
    }
    tags.unwrap_or_default()
        .split(',')
        .map(std::string::ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config;
    use crate::metrics::aggregator::{
        metric::{self, Metric},
        Aggregator, Limit,
    };
    use crate::metrics::constants;
    use crate::tags::provider;
    use crate::LAMBDA_RUNTIME_SLUG;
    use datadog_protos::metrics::SketchPayload;
    use hashbrown::hash_table;
    use protobuf::Message;
    use std::collections::hash_map;
    use std::sync::Arc;

    fn create_tags_provider() -> Arc<provider::Provider> {
        let config = Arc::new(config::Config::default());
        Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &hash_map::HashMap::new(),
        ))
    }

    #[test]
    fn insertion() {
        let mut aggregator = Aggregator::<2>::new(create_tags_provider()).unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        // Both unique contexts get one slot.
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    fn distribution_insertion() {
        let mut aggregator = Aggregator::<2>::new(create_tags_provider()).unwrap();

        let metric1 = Metric::parse("test:1|d|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|d|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        // Both unique contexts get one slot.
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    fn overflow() {
        let mut aggregator = Aggregator::<2>::new(create_tags_provider()).unwrap();

        let metric1 = Metric::parse("test:1|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|c|k:v").expect("metric parse failed");
        let metric3 = Metric::parse("bar:1|c|k:v").expect("metric parse failed");

        let id1 = metric::id(metric1.name, metric1.tags);
        let id2 = metric::id(metric2.name, metric2.tags);
        let id3 = metric::id(metric3.name, metric3.tags);

        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
        assert_ne!(id2, id3);

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
        let mut aggregator = Aggregator::<2>::new(create_tags_provider()).unwrap();

        let metric1 = Metric::parse("test:3|c|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:5|c|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(
            aggregator.get_value_by_id("foo".into(), None).unwrap(),
            5f64
        );
        assert_eq!(
            aggregator.get_value_by_id("test".into(), None).unwrap(),
            3f64
        );
        aggregator.clear();
        assert_eq!(aggregator.map.len(), 0);
    }

    #[test]
    fn to_series() {
        let mut aggregator = Aggregator::<2>::new(create_tags_provider()).unwrap();

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
        let mut aggregator = Aggregator::<2>::new(create_tags_provider()).unwrap();

        let metric1 = Metric::parse("test:1|d|k:v").expect("metric parse failed");
        let metric2 = Metric::parse("foo:1|d|k:v").expect("metric parse failed");

        assert!(aggregator.insert(&metric1).is_ok());
        assert!(aggregator.insert(&metric2).is_ok());

        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.distributions_to_protobuf().sketches().len(), 2);
        assert_eq!(aggregator.map.len(), 2);
        assert_eq!(aggregator.distributions_to_protobuf().sketches().len(), 2);
        assert_eq!(aggregator.map.len(), 2);
    }

    #[test]
    fn distributions_to_protobuf_ignore_single_metrics() {
        let mut aggregator = Aggregator::<1_000>::new(create_tags_provider()).unwrap();
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 0);

        assert!(aggregator
            .insert(
                &Metric::parse("test1:1|d|k:v".to_string().as_str()).expect("metric parse failed")
            )
            .is_ok());
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 1);

        assert!(aggregator
            .insert(&Metric::parse("foo:1|c|k:v").expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.distributions_to_protobuf().sketches.len(), 1);
    }

    #[test]
    fn distributions_to_protobuf_limited_max() {
        let max = 5;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            no_limit: Limit::ApiLimit(usize::MAX, usize::MAX),
            series_limit: Limit::ApiLimit(
                constants::MAX_ENTRIES_SINGLE_METRIC,
                constants::MAX_SIZE_BYTES_SINGLE_METRIC,
            ),
            distribution_limit: Limit::ApiLimit(max, constants::MAX_SIZE_SKETCH_METRIC),
        };

        for i in 1..max * 2 {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|d|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }
        assert_eq!(
            aggregator
                .distributions_to_protobuf_api_limited()
                .sketches
                .len(),
            max
        );
    }

    #[test]
    fn to_series_ignore_distribution() {
        let mut aggregator = Aggregator::<1_000>::new(create_tags_provider()).unwrap();

        assert_eq!(aggregator.to_series().series.len(), 0);

        assert!(aggregator
            .insert(
                &Metric::parse("test1:1|c|k:v".to_string().as_str()).expect("metric parse failed")
            )
            .is_ok());
        assert_eq!(aggregator.to_series().series.len(), 1);

        assert!(aggregator
            .insert(&Metric::parse("foo:1|d|k:v").expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.to_series().series.len(), 1);
    }

    #[test]
    fn to_series_limited_max() {
        let max = 5;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            no_limit: Limit::ApiLimit(usize::MAX, usize::MAX),
            series_limit: Limit::ApiLimit(5, constants::MAX_SIZE_BYTES_SINGLE_METRIC),
            distribution_limit: Limit::ApiLimit(
                constants::MAX_ENTRIES_SKETCH_METRIC,
                constants::MAX_SIZE_SKETCH_METRIC,
            ),
        };

        for i in 1..max * 2 {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|c|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }
        assert_eq!(aggregator.to_series_api_limited().series.len(), max);
    }

    #[test]
    fn distribution_serialized_deserialized() {
        let mut aggregator = Aggregator::<1_000>::new(create_tags_provider()).unwrap();

        for i in 0..10 {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|d|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }
        let distribution = aggregator.distributions_to_protobuf();
        assert_eq!(distribution.sketches().len(), 10);

        let serialized = distribution
            .write_to_bytes()
            .expect("Can't serialized proto");

        let deserialized =
            SketchPayload::parse_from_bytes(serialized.as_slice()).expect("failed to parse proto");

        assert_eq!(deserialized.sketches().len(), 10);
        assert_eq!(deserialized, distribution);
    }
}
