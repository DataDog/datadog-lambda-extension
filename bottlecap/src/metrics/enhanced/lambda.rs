use super::constants::{self, BASE_LAMBDA_INVOCATION_PRICE};
use crate::metrics::aggregator::Aggregator;
use crate::metrics::{errors, metric};
use crate::telemetry::events::ReportMetrics;
use std::env::consts::ARCH;
use std::sync::{Arc, Mutex};
use tracing::error;

pub struct Lambda {
    pub aggregator: Arc<Mutex<Aggregator<1024>>>,
    pub config: Arc<crate::config::Config>,
}

impl Lambda {
    #[must_use]
    pub fn new(
        aggregator: Arc<Mutex<Aggregator<1024>>>,
        config: Arc<crate::config::Config>,
    ) -> Lambda {
        Lambda { aggregator, config }
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
        if !self.config.enhanced_metrics {
            return Ok(());
        }
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
        if !self.config.enhanced_metrics {
            return Ok(());
        }
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
        if !self.config.enhanced_metrics {
            return;
        }
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
        if !self.config.enhanced_metrics {
            return;
        }
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
        if !self.config.enhanced_metrics {
            return;
        }
        let mut aggr: std::sync::MutexGuard<Aggregator<1024>> =
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config;
    use crate::metrics::aggregator::ValueVariant;
    use crate::tags::provider;
    use crate::LAMBDA_RUNTIME_SLUG;
    use ddsketch_agent::DDSketch;
    use std::collections::hash_map::HashMap;
    use std::sync::MutexGuard;

    fn setup() -> (Arc<Mutex<Aggregator<1024>>>, Arc<config::Config>) {
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
        (
            Arc::new(Mutex::new(
                Aggregator::<1024>::new(tags_provider.clone())
                    .expect("failed to create aggregator"),
            )),
            config,
        )
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_increment_invocation_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_invocation_metric().unwrap();
        match metrics_aggr
            .lock()
            .expect("lock poisoned")
            .get_value_by_id(constants::INVOCATIONS_METRIC.into(), None)
        {
            Some(ValueVariant::DDSketch(pbuf)) => assert_eq!(1f64, pbuf.sum().unwrap()),
            _ => panic!("failed to get value by id"),
        };
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn test_increment_errors_metric() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
        lambda.increment_errors_metric().unwrap();
        match metrics_aggr
            .lock()
            .expect("lock poisoned")
            .get_value_by_id(constants::ERRORS_METRIC.into(), None)
        {
            Some(ValueVariant::DDSketch(pbuf)) => assert_eq!(1f64, pbuf.sum().unwrap()),
            _ => panic!("failed to get value by id"),
        };
    }

    #[test]
    fn test_set_report_log_metrics() {
        let (metrics_aggr, my_config) = setup();
        let lambda = Lambda::new(metrics_aggr.clone(), my_config);
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

        assert_value(
            &mut aggr,
            0.1,
            vec![
                constants::DURATION_METRIC,
                constants::BILLED_DURATION_METRIC,
            ],
        );

        assert_value(&mut aggr, 128.0, vec![constants::MAX_MEMORY_USED_METRIC]);
        assert_value(&mut aggr, 256.0, vec![constants::MEMORY_SIZE_METRIC]);
    }

    fn assert_value(
        aggr: &mut MutexGuard<Aggregator<1024>>,
        sketch_val: f64,
        metric_names: Vec<&str>,
    ) {
        let mut ms_sketch = DDSketch::default();
        ms_sketch.insert(sketch_val);

        for a_metric_name in metric_names {
            if let Some(ValueVariant::DDSketch(variant)) =
                aggr.get_value_by_id(a_metric_name.into(), None)
            {
                assert_eq!(variant, ms_sketch);
            } else {
                panic!("failed to get {a_metric_name} by id");
            }
        }
    }
}
