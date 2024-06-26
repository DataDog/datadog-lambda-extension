//! The aggregation of metrics.

use crate::{
    metrics::{constants, datadog, errors, metric},
    tags::provider,
};
use datadog::Metric as DatadogMetric;
use metric::{Metric, Type};
use std::sync::Arc;

use crate::metrics::datadog::Series;
use datadog_protos::metrics::{Dogsketch, Sketch, SketchPayload};
use ddsketch_agent::DDSketch;
use hashbrown::hash_table;
use log::warn;
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
    max_batch_entries_single_metric: usize,
    max_batch_bytes_single_metric: u64,
    max_batch_entries_sketch_metric: usize,
    max_batch_bytes_sketch_metric: u64,
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
            max_batch_entries_single_metric: constants::MAX_ENTRIES_SINGLE_METRIC,
            max_batch_bytes_single_metric: constants::MAX_SIZE_BYTES_SINGLE_METRIC,
            max_batch_entries_sketch_metric: constants::MAX_ENTRIES_SKETCH_METRIC,
            max_batch_bytes_sketch_metric: constants::MAX_SIZE_SKETCH_METRIC,
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
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let mut sketch_payload = SketchPayload::new();
        let base_tag_vec = self.tags_provider.get_tags_vec();

        self.map
            .iter()
            .filter_map(|entry| match entry.metric_value {
                MetricValue::Distribution(_) => build_sketch(now, entry, base_tag_vec.clone()),
                _ => None,
            })
            .for_each(|sketch| sketch_payload.sketches.push(sketch));
        sketch_payload
    }

    #[must_use]
    pub fn consume_distributions(&mut self) -> Vec<SketchPayload> {
        let now = time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        let mut batched_payloads = Vec::new();
        let mut sketch_payload = SketchPayload::new();
        let mut count_bytes = 0u64;
        let base_tag_vec = self.tags_provider.get_tags_vec();
        for sketch in self
            .map
            .extract_if(|entry| {
                if let MetricValue::Distribution(_) = entry.metric_value {
                    return true;
                }
                false
            })
            .filter_map(|entry| build_sketch(now, &entry, base_tag_vec.clone()))
        {
            let next_chunk_size = sketch.compute_size();

            if (sketch_payload.sketches.len() >= self.max_batch_entries_sketch_metric)
                || (count_bytes + next_chunk_size >= self.max_batch_bytes_sketch_metric)
            {
                if count_bytes == 0 {
                    warn!("Only one distribution exceeds max batch size, adding it anyway: {:?} with {}", sketch.metric, next_chunk_size);
                } else {
                    batched_payloads.push(sketch_payload);
                    sketch_payload = SketchPayload::new();
                    count_bytes = 0u64;
                }
            }
            count_bytes += next_chunk_size;
            sketch_payload.sketches.push(sketch);
        }
        if !sketch_payload.sketches.is_empty() {
            batched_payloads.push(sketch_payload);
        }
        batched_payloads
    }

    #[must_use]
    pub fn to_series(&self) -> Series {
        let mut series = Series {
            series: Vec::with_capacity(1_024),
        };

        self.map
            .iter()
            .filter_map(|entry| match entry.metric_value {
                MetricValue::Distribution(_) => None,
                _ => build_metric(entry, self.tags_provider.get_tags_vec()),
            })
            .for_each(|metric| series.series.push(metric));
        series
    }

    #[must_use]
    pub fn consume_metrics(&mut self) -> Vec<Series> {
        let mut batched_payloads = Vec::new();
        let mut series = Series {
            series: Vec::with_capacity(1_024),
        };
        let mut count_bytes = 0u64;
        for metric in self
            .map
            .extract_if(|entry| {
                if let MetricValue::Distribution(_) = entry.metric_value {
                    return false;
                }
                true
            })
            .filter_map(|entry| build_metric(&entry, self.tags_provider.get_tags_vec()))
        {
            let serialized_metric_size = match serde_json::to_vec(&metric) {
                Ok(serialized_metric) => serialized_metric.len() as u64,
                Err(e) => {
                    error!("failed to serialize metric: {:?}", e);
                    0u64
                }
            };

            if serialized_metric_size > 0 {
                if (series.series.len() >= self.max_batch_entries_single_metric)
                    || (count_bytes + serialized_metric_size >= self.max_batch_bytes_single_metric)
                {
                    if count_bytes == 0 {
                        warn!("Only one metric exceeds max batch size, adding it anyway: {:?} with {}", metric.metric, serialized_metric_size);
                    } else {
                        batched_payloads.push(series);
                        series = Series {
                            series: Vec::with_capacity(1_024),
                        };
                        count_bytes = 0u64;
                    }
                }
                series.series.push(metric);
                count_bytes += serialized_metric_size;
            }
        }

        if !series.series.is_empty() {
            batched_payloads.push(series);
        }
        batched_payloads
    }

    #[cfg(test)]
    pub fn get_value_by_id(&mut self, name: Ustr, tags: Option<Ustr>) -> Option<ValueVariant> {
        let id = metric::id(name, tags);

        match self
            .map
            .entry(id, |m| m.id == id, |m| metric::id(m.name, m.tags))
        {
            hash_table::Entry::Vacant(_) => None,
            hash_table::Entry::Occupied(entry) => match entry.get() {
                Entry {
                    metric_value: MetricValue::Count(count),
                    ..
                } => Some(ValueVariant::Value(*count)),
                Entry {
                    metric_value: MetricValue::Gauge(gauge),
                    ..
                } => Some(ValueVariant::Value(*gauge)),
                Entry {
                    metric_value: MetricValue::Distribution(distribution),
                    ..
                } => Some(ValueVariant::DDSketch(distribution.clone())),
            },
        }
    }
}

fn build_sketch(now: i64, entry: &Entry, mut base_tag_vec: Vec<String>) -> Option<Sketch> {
    let sketch = entry.metric_value.get_sketch()?;
    let mut dogsketch = Dogsketch::default();
    sketch.merge_to_dogsketch(&mut dogsketch);
    // TODO(Astuyve) allow users to specify timestamp
    dogsketch.set_ts(now);
    let mut sketch = Sketch::default();
    sketch.set_dogsketches(vec![dogsketch]);
    let name = entry.name.to_string();
    sketch.set_metric(name.clone().into());
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

fn build_metric(entry: &Entry, mut base_tag_vec: Vec<String>) -> Option<DatadogMetric> {
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
        final_tags = tags.split(',').map(ToString::to_string).collect();
    }
    final_tags.append(&mut base_tag_vec);
    Some(DatadogMetric {
        metric: entry.name.as_str(),
        resources,
        kind,
        points: [point; 1],
        tags: final_tags,
    })
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub enum ValueVariant {
    DDSketch(DDSketch),
    Value(f64),
}

fn tags_string_to_vector(tags: Option<Ustr>) -> Vec<String> {
    if tags.is_none() {
        return Vec::new();
    }
    tags.unwrap_or_default()
        .split(',')
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::config;
    use crate::metrics::aggregator::{
        metric::{self, Metric},
        Aggregator, ValueVariant,
    };
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
        if let Some(ValueVariant::Value(v)) = aggregator.get_value_by_id("foo".into(), None) {
            assert_eq!(v, 5f64);
        } else {
            panic!("failed to get value by id");
        }

        if let Some(ValueVariant::Value(v)) = aggregator.get_value_by_id("test".into(), None) {
            assert_eq!(v, 3f64);
        } else {
            panic!("failed to get value by id");
        }

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
    fn consume_distributions_ignore_single_metrics() {
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
    fn consume_distributions_batch_entries() {
        let max_batch = 5;
        let tot = 12;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: 1_000,
            max_batch_entries_sketch_metric: max_batch,
            max_batch_bytes_sketch_metric: 1_500,
        };

        add_metrics(tot, &mut aggregator, "d".to_string());
        let batched = aggregator.consume_distributions();
        assert_eq!(aggregator.consume_distributions().len(), 0);

        assert_eq!(batched.len(), 3);
        assert_eq!(batched.first().unwrap().sketches.len(), max_batch);
        assert_eq!(batched.get(1).unwrap().sketches.len(), max_batch);
        assert_eq!(batched.get(2).unwrap().sketches.len(), tot - max_batch * 2);
    }

    #[test]
    fn consume_distributions_batch_bytes() {
        let single_proto_size = 102;
        let max_bytes = 250;
        let tot = 5;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: 1_000,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: max_bytes,
        };

        add_metrics(tot, &mut aggregator, "d".to_string());
        let batched = aggregator.consume_distributions();

        assert_eq!(batched.len(), tot / 2 + 1);
        assert_eq!(
            batched.first().unwrap().compute_size(),
            single_proto_size * 2
        );
        assert_eq!(
            batched.get(1).unwrap().compute_size(),
            single_proto_size * 2
        );
        assert_eq!(batched.get(2).unwrap().compute_size(), single_proto_size);
    }

    #[test]
    fn consume_distribution_one_element_bigger_than_max_size() {
        let single_proto_size = 102;
        let max_bytes = 1;
        let tot = 5;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: 1_000,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: max_bytes,
        };

        add_metrics(tot, &mut aggregator, "d".to_string());
        let batched = aggregator.consume_distributions();

        assert_eq!(batched.len(), tot);
        for a_batch in batched {
            assert_eq!(a_batch.compute_size(), single_proto_size);
        }
    }

    fn add_metrics(tot: usize, aggregator: &mut Aggregator<1000>, counter_or_distro: String) {
        for i in 1..=tot {
            assert!(aggregator
                .insert(
                    &Metric::parse(format!("test{i}:{i}|{counter_or_distro}|k:v").as_str())
                        .expect("metric parse failed")
                )
                .is_ok());
        }
    }

    #[test]
    fn consume_series_ignore_distribution() {
        let mut aggregator = Aggregator::<1_000>::new(create_tags_provider()).unwrap();

        assert_eq!(aggregator.consume_metrics().len(), 0);

        assert!(aggregator
            .insert(
                &Metric::parse("test1:1|c|k:v".to_string().as_str()).expect("metric parse failed")
            )
            .is_ok());
        assert_eq!(aggregator.consume_distributions().len(), 0);
        assert_eq!(aggregator.consume_metrics().len(), 1);
        assert_eq!(aggregator.consume_metrics().len(), 0);

        assert!(aggregator
            .insert(
                &Metric::parse("test1:1|c|k:v".to_string().as_str()).expect("metric parse failed")
            )
            .is_ok());
        assert!(aggregator
            .insert(&Metric::parse("foo:1|d|k:v").expect("metric parse failed"))
            .is_ok());
        assert_eq!(aggregator.consume_metrics().len(), 1);
        assert_eq!(aggregator.consume_distributions().len(), 1);
        assert_eq!(aggregator.consume_distributions().len(), 0);
    }

    #[test]
    fn consume_series_batch_entries() {
        let max_batch = 5;
        let tot = 13;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: max_batch,
            max_batch_bytes_single_metric: 10_000,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: 1_500,
        };

        add_metrics(tot, &mut aggregator, "c".to_string());

        let batched = aggregator.consume_metrics();
        assert_eq!(batched.len(), 3);
        assert_eq!(batched.first().unwrap().series.len(), max_batch);
        assert_eq!(batched.get(1).unwrap().series.len(), max_batch);
        assert_eq!(batched.get(2).unwrap().series.len(), tot - max_batch * 2);

        assert_eq!(aggregator.consume_metrics().len(), 0);
    }

    #[test]
    fn consume_metrics_batch_bytes() {
        let single_metric_size = 156;
        let two_metrics_size = 300;
        let max_bytes = 350;
        let tot = 5;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: max_bytes,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: 1_000,
        };

        add_metrics(tot, &mut aggregator, "c".to_string());
        let batched = aggregator.consume_metrics();

        assert_eq!(batched.len(), tot / 2 + 1);
        assert_eq!(
            serde_json::to_vec(batched.first().unwrap()).unwrap().len(),
            two_metrics_size
        );
        assert_eq!(
            serde_json::to_vec(batched.get(1).unwrap()).unwrap().len(),
            two_metrics_size
        );
        assert_eq!(
            serde_json::to_vec(batched.get(2).unwrap()).unwrap().len(),
            single_metric_size
        );
    }

    #[test]
    fn consume_series_one_element_bigger_than_max_size() {
        let single_metric_size = 156;
        let max_bytes = 1;
        let tot = 5;
        let mut aggregator = Aggregator::<1_000> {
            tags_provider: create_tags_provider(),
            map: hash_table::HashTable::new(),
            max_batch_entries_single_metric: 1_000,
            max_batch_bytes_single_metric: max_bytes,
            max_batch_entries_sketch_metric: 1_000,
            max_batch_bytes_sketch_metric: 1_000,
        };

        add_metrics(tot, &mut aggregator, "c".to_string());
        let batched = aggregator.consume_metrics();

        assert_eq!(batched.len(), tot);
        for a_batch in batched {
            assert_eq!(
                serde_json::to_vec(&a_batch).unwrap().len(),
                single_metric_size
            );
        }
    }

    #[test]
    fn distribution_serialized_deserialized() {
        let mut aggregator = Aggregator::<1_000>::new(create_tags_provider()).unwrap();

        add_metrics(10, &mut aggregator, "d".to_string());
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
