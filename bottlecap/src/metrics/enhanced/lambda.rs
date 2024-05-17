use super::constants;
use crate::metrics::aggregator::Aggregator;
use crate::metrics::{errors, metric};
use std::sync::{Arc, Mutex};

pub struct Lambda {
    pub aggregator: Arc<Mutex<Aggregator<1024>>>,
}

impl Lambda {
    #[must_use]
    pub fn new(aggregator: Arc<Mutex<Aggregator<1024>>>) -> Lambda {
        Lambda { aggregator }
    }

    pub fn increment_invocation_metric(&self) -> Result<(), errors::Insert> {
        self.increment_metric(constants::INVOCATIONS_METRIC)
    }

    pub fn increment_errors_metric(&self) -> Result<(), errors::Insert> {
        self.increment_metric(constants::ERRORS_METRIC)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::tags::provider;
    use crate::LAMBDA_RUNTIME_SLUG;
    use std::collections::hash_map::HashMap;

    fn setup() -> Arc<Mutex<Aggregator<1024>>> {
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
            Aggregator::<1024>::new(tags_provider.clone()).expect("failed to create aggregator"),
        ))
    }

    #[test]
    fn test_increment_invocation_metric() {
        let metrics_aggr = setup();
        let lambda = Lambda::new(metrics_aggr.clone());
        lambda.increment_invocation_metric().unwrap();
        let pbuf = metrics_aggr
            .lock()
            .expect("lock poisoned")
            .distributions_to_protobuf();
        let _ = pbuf.sketches().iter().map(|sketch| {
            assert_eq!(sketch.metric, constants::INVOCATIONS_METRIC.into());
        });
    }

    #[test]
    fn test_increment_errors_metric() {
        let metrics_aggr = setup();
        let lambda = Lambda::new(metrics_aggr.clone());
        lambda.increment_errors_metric().unwrap();
        let pbuf = metrics_aggr
            .lock()
            .expect("lock poisoned")
            .distributions_to_protobuf();
        let _ = pbuf.sketches().iter().map(|sketch| {
            assert_eq!(sketch.metric, constants::ERRORS_METRIC.into());
        });
    }
}
