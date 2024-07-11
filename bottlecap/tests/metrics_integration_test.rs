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
    let dd_api_key = "my_test_key";

    // payload looks like
    // aws.lambda.enhanced.invocations"_dd.compute_stats:1"architecture:x86_64"function_arn:test-arn:�൴      �?!      �?)      �?1      �?:�B
    // protobuf is using hashmap, can't set a btreemap to have sorted keys. Using multiple regexp since
    // Can't do look around since -> error: look-around, including look-ahead and look-behind, is not supported
    let regexp_metric_name = r#"aws.lambda.enhanced.invocations"#;
    let regexp_compute_state = r#"_dd.compute_stats:1"#;
    let regexp_arch = r#"architecture:x86_64"#;
    let regexp_function_arn = r#"function_arn:test-arn"#;

    let server = MockServer::start();
    let hello_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/beta/sketches")
            .header("DD-API-KEY", dd_api_key)
            .header("Content-Type", "application/x-protobuf")
            .body_contains(regexp_metric_name)
            .body_contains(regexp_compute_state)
            .body_contains(regexp_arch)
            .body_contains(regexp_function_arn);
        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });

    let arc_config = Arc::new(Config::default());
    let provider = Arc::new(Provider::new(
        Arc::clone(&arc_config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
    ));

    let metrics_aggr = Arc::new(Mutex::new(
        MetricsAggregator::new(provider.clone(), 1024).expect("failed to create aggregator"),
    ));
    let mut metrics_flusher = MetricsFlusher::new(
        dd_api_key.to_string(),
        metrics_aggr.clone(),
        server.base_url(),
    );
    let lambda_enhanced_metrics =
        enhanced_metrics::new(Arc::clone(&metrics_aggr), Arc::clone(&arc_config));

    lambda_enhanced_metrics.increment_invocation_metric();

    let _ = metrics_flusher.flush().await;

    hello_mock.assert();
}
