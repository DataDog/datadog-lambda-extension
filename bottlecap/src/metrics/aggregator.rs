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

#[derive(Clone)]
// NOTE by construction we know that intervals and contexts do not explore the
// full space of usize but the type system limits how we can express this today.
pub struct Aggregator<const CONTEXTS: usize> {
    tags_provider: Arc<provider::Provider>,
    map: hash_table::HashTable<Entry>,
    max_batch_entries_size: usize,
    max_content_size_bytes: usize,
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
            max_batch_entries_size: constants::MAX_ENTRIES_NUMBER,
            max_content_size_bytes: constants::MAX_CONTENT_SIZE_BYTES,
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
        let mut sketch_payload = SketchPayload::new();
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        for entry in &self.map {
            let Some(sketch) = self.build_sketch(now, entry) else {
                continue;
            };
            sketch_payload.sketches.push(sketch);
        }
        sketch_payload
    }

    #[must_use]
    pub fn distributions_to_protobuf_serialized(&self) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::with_capacity(self.max_content_size_bytes);
        let mut count_entries = 0;
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        for entry in &self.map {
            let Some(sketch) = self.build_sketch(now, entry) else {
                continue;
            };
            if sketch.compute_size() + buffer.len() as u64 > self.max_content_size_bytes as u64 {
                break;
            }

            let serialized_metric = match sketch.write_to_bytes() {
                Ok(serialized_sketch) => serialized_sketch,
                Err(e) => {
                    error!("failed to serialize metric: {:?}", e);
                    continue;
                }
            };

            buffer.extend(serialized_metric);
            count_entries += 1;

            if count_entries >= self.max_batch_entries_size {
                break;
            }
        }
        buffer
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

    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn to_series(&self) -> datadog::Series {
        // TODO it would be really slick to use a bump allocator here since
        // there's so many tiny allocations
        let mut series = datadog::Series {
            series: Vec::with_capacity(1_024),
        };
        for entry in &self.map {
            let Some(metric) = self.build_metric(entry) else {
                continue;
            };
            series.series.push(metric);
        }
        series
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

    #[allow(clippy::cast_precision_loss)]
    #[must_use]
    pub fn to_series_serialized(&self) -> Vec<u8> {
        let mut buffer: Vec<u8> = Vec::with_capacity(self.max_content_size_bytes);
        let mut count_entries = 0;
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
            if serialized_metric.len() + buffer.len() > self.max_content_size_bytes {
                break;
            }
            buffer.extend(serialized_metric);
            count_entries += 1;

            if count_entries >= self.max_batch_entries_size {
                break;
            }
        }
        buffer
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
        Aggregator,
    };
    use crate::tags::provider;
    use crate::LAMBDA_RUNTIME_SLUG;
    use hashbrown::hash_table;
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
    fn distributions_to_protobuf_serialized() {
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_size: 100,
            max_content_size_bytes: 1500,
        };

        assert_eq!(aggregator.distributions_to_protobuf_serialized().len(), 0);

        assert!(aggregator
            .insert(
                &Metric::parse("test1:1|d|k:v".to_string().as_str()).expect("metric parse failed")
            )
            .is_ok());
        assert_eq!(aggregator.distributions_to_protobuf_serialized().len(), 100);

        assert!(aggregator
            .insert(&Metric::parse("foo:1|c|k:v").expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.distributions_to_protobuf_serialized().len(), 100);

        for i in 10..20 {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|d|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }

        assert_eq!(
            aggregator.distributions_to_protobuf_serialized().len(),
            1110
        );

        aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_size: 5,
            max_content_size_bytes: 1500,
        };

        for i in 10..20 {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|d|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }
        assert_eq!(aggregator.distributions_to_protobuf_serialized().len(), 505);
    }

    #[test]
    fn to_series_serialized() {
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_size: 100,
            max_content_size_bytes: 1500,
        };

        assert_eq!(aggregator.distributions_to_protobuf_serialized().len(), 0);

        assert!(aggregator
            .insert(
                &Metric::parse("test1:1|c|k:v".to_string().as_str()).expect("metric parse failed")
            )
            .is_ok());
        assert_eq!(aggregator.to_series_serialized().len(), 143);

        assert!(aggregator
            .insert(&Metric::parse("foo:1|d|k:v").expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.to_series_serialized().len(), 143);

        for i in 10..20 {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|c|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }

        assert_eq!(aggregator.to_series_serialized().len(), 1448);

        aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_size: 5,
            max_content_size_bytes: 1500,
        };

        for i in 10..20 {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|c|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }
        assert_eq!(aggregator.to_series_serialized().len(), 725);
    }
}
