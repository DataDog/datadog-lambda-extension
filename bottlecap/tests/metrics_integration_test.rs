use bottlecap::config::Config;
use bottlecap::metrics::aggregator::Aggregator as MetricsAggregator;
use bottlecap::metrics::enhanced::lambda::Lambda as enhanced_metrics;
use bottlecap::metrics::flusher::Flusher as MetricsFlusher;
use bottlecap::tags::provider::Provider;
use bottlecap::LAMBDA_RUNTIME_SLUG;
use httpmock::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod common;

#[tokio::test]
async fn test_enhanced_metrics() {
    let server = MockServer::start();
    let hello_mock = server.mock(|when, then| {
        when.method(POST).path("/api/beta/sketches");
        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });

    let provider = Arc::new(Provider::new(
        Arc::new(Config::default()),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
    ));

    let metrics_aggr = Arc::new(Mutex::new(
        MetricsAggregator::new(provider.clone(), 1024).expect("failed to create aggregator"),
    ));
    let mut metrics_flusher =
        MetricsFlusher::new("key".to_string(), metrics_aggr.clone(), server.base_url());
    let lambda_enhanced_metrics = enhanced_metrics::new(Arc::clone(&metrics_aggr));

    lambda_enhanced_metrics
        .increment_invocation_metric()
        .expect("failed to increment invocation metric");

    let res = metrics_flusher.flush().await;
    for r in res {
        assert!(r.1.is_none());
    }

    hello_mock.assert();
}
