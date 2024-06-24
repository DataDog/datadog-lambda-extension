use super::constants::{self, BASE_LAMBDA_INVOCATION_PRICE};
use crate::metrics::aggregator::Aggregator;
use crate::metrics::{errors, metric};
use crate::telemetry::events::ReportMetrics;
use std::env::consts::ARCH;
use std::sync::{Arc, Mutex};
use tracing::error;

pub struct Lambda {
    pub aggregator: Arc<Mutex<Aggregator>>,
}

impl Lambda {
    #[must_use]
    pub fn new(aggregator: Arc<Mutex<Aggregator>>) -> Lambda {
        Lambda { aggregator }
    }

    pub fn increment_invocation_metric(&self) -> Result<(), errors::Insert> {
        self.increment_metric(constants::INVOCATIONS_METRIC)
    }

    pub fn increment_errors_metric(&self) -> Result<(), errors::Insert> {
        self.increment_metric(constants::ERRORS_METRIC)
    }

    pub fn increment_timeout_metric(&self) -> Result<(), errors::Insert> {
        self.increment_metric(constants::TIMEOUTS_METRIC)
    }

    pub fn set_init_duration_metric(&self, init_duration_ms: f64) -> Result<(), errors::Insert> {
        let metric = metric::Metric::new(
            constants::INIT_DURATION_METRIC.into(),
            metric::Type::Distribution,
            (init_duration_ms * constants::MS_TO_SEC).to_string().into(),
            None,
        );
        self.aggregator
            .lock()
            .expect("lock poisoned")
            .insert(&metric)
    }

    fn increment_metric(&self, metric_name: &str) -> Result<(), errors::Insert> {
        let metric = metric::Metric::new(
            metric_name.into(),
            metric::Type::Distribution,
            "1".into(),
            None,
        );
        self.aggregator
            .lock()
            .expect("lock poisoned")
            .insert(&metric)
    }

    pub fn set_runtime_duration_metric(&self, duration_ms: f64) {
        let metric = metric::Metric::new(
            constants::RUNTIME_DURATION_METRIC.into(),
            metric::Type::Distribution,
            // Datadog expects this value as milliseconds, not seconds
            duration_ms.to_string().into(),
            None,
        );
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(&metric)
        {
            error!("failed to insert runtime duration metric: {}", e);
        }
    }

    pub fn set_post_runtime_duration_metric(&self, duration_ms: f64) {
        let metric = metric::Metric::new(
            constants::POST_RUNTIME_DURATION_METRIC.into(),
            metric::Type::Distribution,
            // Datadog expects this value as milliseconds, not seconds
            duration_ms.to_string().into(),
            None,
        );
        if let Err(e) = self
            .aggregator
            .lock()
            .expect("lock poisoned")
            .insert(&metric)
        {
            error!("failed to insert post runtime duration metric: {}", e);
        }
    }

    fn calculate_estimated_cost_usd(billed_duration_ms: u64, memory_size_mb: u64) -> f64 {
        let gb_seconds = (billed_duration_ms as f64 * constants::MS_TO_SEC)
            * (memory_size_mb as f64 / constants::MB_TO_GB);

        let price_per_gb = match ARCH {
            "x86_64" => constants::X86_LAMBDA_PRICE_PER_GB_SECOND,
            "aarch64" => constants::ARM_LAMBDA_PRICE_PER_GB_SECOND,
            _ => {
                error!("unsupported architecture: {}", ARCH);
                return 0.0;
            }
        };

        ((BASE_LAMBDA_INVOCATION_PRICE + (gb_seconds * price_per_gb)) * 1e12).round() / 1e12
    }

    pub fn set_report_log_metrics(&self, metrics: &ReportMetrics) {
        let mut aggr: std::sync::MutexGuard<Aggregator> =
            self.aggregator.lock().expect("lock poisoned");
        let metric = metric::Metric::new(
            constants::DURATION_METRIC.into(),
            metric::Type::Distribution,
            (metrics.duration_ms * constants::MS_TO_SEC)
                .to_string()
                .into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::BILLED_DURATION_METRIC.into(),
            metric::Type::Distribution,
            (metrics.billed_duration_ms as f64 * constants::MS_TO_SEC)
                .to_string()
                .into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert billed duration metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MAX_MEMORY_USED_METRIC.into(),
            metric::Type::Distribution,
            (metrics.max_memory_used_mb as f64).to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert max memory used metric: {}", e);
        }
        let metric = metric::Metric::new(
            constants::MEMORY_SIZE_METRIC.into(),
            metric::Type::Distribution,
            (metrics.memory_size_mb as f64).to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert memory size metric: {}", e);
        }

        let cost_usd =
            Self::calculate_estimated_cost_usd(metrics.billed_duration_ms, metrics.memory_size_mb);
        let metric = metric::Metric::new(
            constants::ESTIMATED_COST_METRIC.into(),
            metric::Type::Distribution,
            cost_usd.to_string().into(),
            None,
        );
        if let Err(e) = aggr.insert(&metric) {
            error!("failed to insert estimated cost metric: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::tags::provider;
    use crate::LAMBDA_RUNTIME_SLUG;
    use std::collections::hash_map::HashMap;

    fn setup() -> Arc<Mutex<Aggregator>> {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });
        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        Arc::new(Mutex::new(
            Aggregator::new(tags_provider.clone(), 1024).expect("failed to create aggregator"),
        ))
    }

    #[test]
    fn test_increment_invocation_metric() {
        let metrics_aggr = setup();
        let lambda = Lambda::new(metrics_aggr.clone());
        lambda.increment_invocation_metric().unwrap();
        match metrics_aggr
            .lock()
            .expect("lock poisoned")
            .get_sketch_by_id(constants::INVOCATIONS_METRIC.into(), None)
        {
            Some(pbuf) => assert_eq!(1f64, pbuf.sum().unwrap()),
            None => panic!("failed to get value by id"),
        };
    }

    #[test]
    fn test_increment_errors_metric() {
        let metrics_aggr = setup();
        let lambda = Lambda::new(metrics_aggr.clone());
        lambda.increment_errors_metric().unwrap();
        match metrics_aggr
            .lock()
            .expect("lock poisoned")
            .get_sketch_by_id(constants::ERRORS_METRIC.into(), None)
        {
            Some(pbuf) => assert_eq!(1f64, pbuf.sum().unwrap()),
            None => panic!("failed to get value by id"),
        };
    }

    #[test]
    fn test_set_report_log_metrics() {
        let metrics_aggr = setup();
        let lambda = Lambda::new(metrics_aggr.clone());
        let report_metrics = ReportMetrics {
            duration_ms: 100.0,
            billed_duration_ms: 100,
            max_memory_used_mb: 128,
            memory_size_mb: 256,
            init_duration_ms: Some(50.0),
            restore_duration_ms: None,
        };
        lambda.set_report_log_metrics(&report_metrics);
        let mut aggr = metrics_aggr.lock().expect("lock poisoned");

        let mut ms_sketch = ddsketch_agent::DDSketch::default();
        ms_sketch.insert(0.1);
        assert_eq!(
            aggr.get_sketch_by_id(constants::DURATION_METRIC.into(), None)
                .unwrap(),
            ms_sketch
        );
        assert_eq!(
            aggr.get_sketch_by_id(constants::BILLED_DURATION_METRIC.into(), None)
                .unwrap(),
            ms_sketch
        );
        let mut mem_used_sketch = ddsketch_agent::DDSketch::default();
        mem_used_sketch.insert(128.0);
        assert_eq!(
            aggr.get_sketch_by_id(constants::MAX_MEMORY_USED_METRIC.into(), None)
                .unwrap(),
            mem_used_sketch
        );
        let mut max_mem_sketch = ddsketch_agent::DDSketch::default();
        max_mem_sketch.insert(256.0);
        assert_eq!(
            aggr.get_sketch_by_id(constants::MEMORY_SIZE_METRIC.into(), None)
                .unwrap(),
            max_mem_sketch
        );
    }
}
