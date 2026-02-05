use bottlecap::config::Config;
use bottlecap::metrics::enhanced::lambda::Lambda as enhanced_metrics;
use dogstatsd::aggregator_service::AggregatorService;
use dogstatsd::api_key::ApiKeyFactory;
use dogstatsd::datadog::{DdDdUrl, MetricsIntakeUrlPrefix, MetricsIntakeUrlPrefixOverride};
use dogstatsd::flusher::Flusher as MetricsFlusher;
use dogstatsd::flusher::FlusherConfig as MetricsFlusherConfig;
use dogstatsd::metric::SortedTags;
use httpmock::prelude::*;
use std::sync::Arc;

mod common;

#[tokio::test]
async fn test_enhanced_metrics() {
    let dd_api_key = "my_test_key";

    // payload looks like
    // aws.lambda.enhanced.invocations"_dd.compute_stats:1"architecture:x86_64"function_arn:test-arn:�൴      �?!      �?)      �?1      �?:�B
    // protobuf is using hashmap, can't set a btreemap to have sorted keys. Using multiple regexp since
    // Can't do look around since -> error: look-around, including look-ahead and look-behind, is not supported
    let regexp_metric_name = r#"aws.lambda.enhanced.invocations"#;
    let regexp_tags = r#"aTagKey:aTagValue"#;

    let server = MockServer::start();
    let hello_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/api/beta/sketches")
            .header("DD-API-KEY", dd_api_key)
            .header("Content-Type", "application/x-protobuf")
            .body_contains(regexp_metric_name)
            .body_contains(regexp_tags);
        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });

    let arc_config = Arc::new(Config::default());

    let (metrics_aggr_service, metrics_aggr_handle) =
        AggregatorService::new(SortedTags::parse("aTagKey:aTagValue").unwrap(), 1024)
            .expect("failed to create aggregator service");

    tokio::spawn(metrics_aggr_service.run());

    let metrics_site_override = MetricsIntakeUrlPrefixOverride::maybe_new(
        None,
        Some(DdDdUrl::new(server.base_url()).expect("failed to create dd url")),
    )
    .expect("failed to create metrics override");
    let flusher_config = MetricsFlusherConfig {
        api_key_factory: Arc::new(ApiKeyFactory::new(dd_api_key)),
        aggregator_handle: metrics_aggr_handle.clone(),
        metrics_intake_url_prefix: MetricsIntakeUrlPrefix::new(None, Some(metrics_site_override))
            .expect("can't parse metrics intake URL from site"),
        https_proxy: None,
        ca_cert_path: None,
        timeout: std::time::Duration::from_secs(5),
        retry_strategy: dogstatsd::datadog::RetryStrategy::Immediate(1),
        compression_level: 6,
    };
    let metrics_flusher = MetricsFlusher::new(flusher_config);
    let lambda_enhanced_metrics =
        enhanced_metrics::new(metrics_aggr_handle.clone(), Arc::clone(&arc_config));

    let now = std::time::UNIX_EPOCH
        .elapsed()
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap_or_default();
    lambda_enhanced_metrics.increment_invocation_metric(now);

    let _ = metrics_flusher.flush().await;

    hello_mock.assert();
}
