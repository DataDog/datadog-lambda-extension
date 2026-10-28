//! Shared setup for the criterion benches: the production allocator, a Lambda-like config
//! and tags provider, and a 2-worker runtime (Lambda exposes 2 vCPUs).
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bottlecap::LAMBDA_RUNTIME_SLUG;
use bottlecap::config::Config;
use bottlecap::tags::provider::Provider;
use criterion::Criterion;

/// Production uses jemalloc; measure with it.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

pub use crate::fixtures::telemetry::FUNCTION_ARN;

/// Sets the AWS variables the tags provider and the config read.
pub fn setup_env() {
    for (k, v) in [
        ("AWS_LAMBDA_FUNCTION_NAME", "orders-prod-handler"),
        ("AWS_LAMBDA_FUNCTION_VERSION", "$LATEST"),
        ("AWS_LAMBDA_FUNCTION_MEMORY_SIZE", "128"),
        ("AWS_EXECUTION_ENV", "AWS_Lambda_python3.12"),
        ("AWS_LAMBDA_INITIALIZATION_TYPE", "on-demand"),
        ("AWS_REGION", "us-east-1"),
    ] {
        // Benches are single-threaded at setup time.
        unsafe { std::env::set_var(k, v) };
    }
}

/// A typical customer config: service, env, version and 4 custom tags.
pub fn config() -> Arc<Config> {
    let mut config = Config {
        service: Some("orders".to_string()),
        env: Some("prod".to_string()),
        version: Some("1.42.0".to_string()),
        tags: HashMap::from([
            ("env".to_string(), "prod".to_string()),
            ("team".to_string(), "checkout".to_string()),
            ("cost_center".to_string(), "cc-1234".to_string()),
            ("owner".to_string(), "payments-platform".to_string()),
        ]),
        apm_dd_url: "https://trace.agent.datadoghq.com".to_string(),
        ..Config::default()
    };
    config.ext.serverless_logs_enabled = true;
    Arc::new(config)
}

pub fn provider(config: Arc<Config>) -> Arc<Provider> {
    Arc::new(Provider::new(
        config,
        LAMBDA_RUNTIME_SLUG.to_string(),
        &HashMap::from([("function_arn".to_string(), FUNCTION_ARN.to_string())]),
    ))
}

/// A multi-thread runtime with 2 workers, like the extension's.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

/// Criterion with the suite's defaults. `cargo bench -- --quick` still shortens runs.
pub fn criterion() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3))
        .sample_size(50)
        .configure_from_args()
}
