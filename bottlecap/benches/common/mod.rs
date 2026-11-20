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

use crate::fixtures::invocation::{
    ENV, FUNCTION_ARN, FUNCTION_NAME, FUNCTION_VERSION, INIT_TYPE, MEMORY_SIZE_MB, REGION, SERVICE,
    VERSION,
};

/// Production uses jemalloc; measure with it.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// The variables Lambda gives only to the runtime process, not to extensions (Extensions API
/// docs), and the per-invocation `_X_AMZN_TRACE_ID`. An inherited `AWS_EXECUTION_ENV` adds a
/// `runtime` tag to every span and log line.
const RUNTIME_ONLY: &[&str] = &[
    "AWS_EXECUTION_ENV",
    "AWS_LAMBDA_LOG_GROUP_NAME",
    "AWS_LAMBDA_LOG_STREAM_NAME",
    "AWS_XRAY_CONTEXT_MISSING",
    "AWS_XRAY_DAEMON_ADDRESS",
    "LAMBDA_RUNTIME_DIR",
    "LAMBDA_TASK_ROOT",
    "_AWS_XRAY_DAEMON_ADDRESS",
    "_AWS_XRAY_DAEMON_PORT",
    "_HANDLER",
    "_X_AMZN_TRACE_ID",
];

/// Sets the AWS variables the tags provider and the config read, for the fixture function, and
/// removes the `RUNTIME_ONLY` ones the host shell may have.
pub fn setup_env() {
    for k in RUNTIME_ONLY {
        // Benches are single-threaded at setup time.
        unsafe { std::env::remove_var(k) };
    }
    for (k, v) in [
        ("AWS_LAMBDA_FUNCTION_NAME", FUNCTION_NAME.to_string()),
        ("AWS_LAMBDA_FUNCTION_VERSION", FUNCTION_VERSION.to_string()),
        (
            "AWS_LAMBDA_FUNCTION_MEMORY_SIZE",
            MEMORY_SIZE_MB.to_string(),
        ),
        ("AWS_LAMBDA_INITIALIZATION_TYPE", INIT_TYPE.to_string()),
        ("AWS_REGION", REGION.to_string()),
    ] {
        // Benches are single-threaded at setup time.
        unsafe { std::env::set_var(k, v) };
    }
}

/// A typical customer config: the fixture service, env and version, and 4 custom tags.
pub fn config() -> Arc<Config> {
    let mut config = Config {
        service: Some(SERVICE.to_string()),
        env: Some(ENV.to_string()),
        version: Some(VERSION.to_string()),
        tags: HashMap::from([
            ("env".to_string(), ENV.to_string()),
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
