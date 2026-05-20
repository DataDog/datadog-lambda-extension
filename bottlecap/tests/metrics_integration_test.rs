use bottlecap::config::Config;
use bottlecap::metrics::enhanced::lambda::Lambda as enhanced_metrics;
use dogstatsd::aggregator::AggregatorService;
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
        client: datadog_fips::reqwest_adapter::create_reqwest_client_builder()
            .expect("failed to create client builder")
            .build()
            .expect("failed to build client"),
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

#[tokio::test]
async fn test_customer_metrics_exclude_tags() {
    let dd_api_key = "my_test_key";

    let all_tags = "function_arn:test-arn,region:us-east-1,account_id:123456789012";
    let filtered_tags = "account_id:123456789012";

    let regexp_metric_name = r#"aws.lambda.enhanced.invocations"#;

    // --- Unfiltered pipeline: all enrichment tags are present ---
    let server_unfiltered = MockServer::start();
    let mock_unfiltered = server_unfiltered.mock(|when, then| {
        when.method(POST)
            .path("/api/beta/sketches")
            .header("DD-API-KEY", dd_api_key)
            .header("Content-Type", "application/x-protobuf")
            .body_contains(regexp_metric_name)
            .body_contains("function_arn:test-arn")
            .body_contains("region:us-east-1")
            .body_contains("account_id:123456789012");
        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });

    // --- Filtered pipeline: excluded tags are absent ---
    let server_filtered = MockServer::start();
    let mock_filtered_with_kept_tag = server_filtered.mock(|when, then| {
        when.method(POST)
            .path("/api/beta/sketches")
            .header("DD-API-KEY", dd_api_key)
            .header("Content-Type", "application/x-protobuf")
            .body_contains(regexp_metric_name)
            .body_contains("account_id:123456789012");
        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });
    let mock_filtered_should_not_have_function_arn = server_filtered.mock(|when, then| {
        when.method(POST)
            .path("/api/beta/sketches")
            .body_contains("function_arn:test-arn");
        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });
    let mock_filtered_should_not_have_region = server_filtered.mock(|when, then| {
        when.method(POST)
            .path("/api/beta/sketches")
            .body_contains("region:us-east-1");
        then.status(reqwest::StatusCode::ACCEPTED.as_u16());
    });

    let arc_config = Arc::new(Config::default());

    // Unfiltered aggregator — simulates Lambda without DD_CUSTOMER_METRICS_EXCLUDE_TAGS
    let (unfiltered_aggr_service, unfiltered_aggr_handle) =
        AggregatorService::new(SortedTags::parse(all_tags).unwrap(), 1024)
            .expect("failed to create unfiltered aggregator");
    tokio::spawn(unfiltered_aggr_service.run());

    // Filtered aggregator — simulates DD_CUSTOMER_METRICS_EXCLUDE_TAGS=function_arn,region
    let (filtered_aggr_service, filtered_aggr_handle) =
        AggregatorService::new(SortedTags::parse(filtered_tags).unwrap(), 1024)
            .expect("failed to create filtered aggregator");
    tokio::spawn(filtered_aggr_service.run());

    // Flushers
    let unfiltered_override = MetricsIntakeUrlPrefixOverride::maybe_new(
        None,
        Some(DdDdUrl::new(server_unfiltered.base_url()).expect("failed to create dd url")),
    )
    .expect("failed to create metrics override");
    let unfiltered_flusher = MetricsFlusher::new(MetricsFlusherConfig {
        api_key_factory: Arc::new(ApiKeyFactory::new(dd_api_key)),
        aggregator_handle: unfiltered_aggr_handle.clone(),
        metrics_intake_url_prefix: MetricsIntakeUrlPrefix::new(None, Some(unfiltered_override))
            .expect("can't parse metrics intake URL"),
        client: datadog_fips::reqwest_adapter::create_reqwest_client_builder()
            .expect("failed to create client builder")
            .build()
            .expect("failed to build client"),
        retry_strategy: dogstatsd::datadog::RetryStrategy::Immediate(1),
        compression_level: 6,
    });

    let filtered_override = MetricsIntakeUrlPrefixOverride::maybe_new(
        None,
        Some(DdDdUrl::new(server_filtered.base_url()).expect("failed to create dd url")),
    )
    .expect("failed to create metrics override");
    let filtered_flusher = MetricsFlusher::new(MetricsFlusherConfig {
        api_key_factory: Arc::new(ApiKeyFactory::new(dd_api_key)),
        aggregator_handle: filtered_aggr_handle.clone(),
        metrics_intake_url_prefix: MetricsIntakeUrlPrefix::new(None, Some(filtered_override))
            .expect("can't parse metrics intake URL"),
        client: datadog_fips::reqwest_adapter::create_reqwest_client_builder()
            .expect("failed to create client builder")
            .build()
            .expect("failed to build client"),
        retry_strategy: dogstatsd::datadog::RetryStrategy::Immediate(1),
        compression_level: 6,
    });

    // Both Lambda functions emit the same enhanced metric
    let unfiltered_enhanced =
        enhanced_metrics::new(unfiltered_aggr_handle.clone(), Arc::clone(&arc_config));
    let filtered_enhanced =
        enhanced_metrics::new(filtered_aggr_handle.clone(), Arc::clone(&arc_config));

    let now = std::time::UNIX_EPOCH
        .elapsed()
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap_or_default();

    unfiltered_enhanced.increment_invocation_metric(now);
    filtered_enhanced.increment_invocation_metric(now);

    let _ = unfiltered_flusher.flush().await;
    let _ = filtered_flusher.flush().await;

    // Unfiltered pipeline: all tags present
    mock_unfiltered.assert();

    // Filtered pipeline: kept tag present, excluded tags absent
    mock_filtered_with_kept_tag.assert();
    mock_filtered_should_not_have_function_arn.assert_hits(0);
    mock_filtered_should_not_have_region.assert_hits(0);
}
