//! Test-mode entry point for the bottlecap APM trace processor.
//!
//! Runs the trace-processing surface (accept -> aggregate -> flush) as a
//! long-lived HTTP server with no AWS Lambda Extension lifecycle. Intended for
//! the cross-agent parity harness ([APMSVLS-496]) and for local developer
//! workflows that need to point a tracer at bottlecap without standing up a
//! Lambda.
//!
//! Endpoints exposed on `127.0.0.1:8126`:
//!
//! | Path           | Method    | Source                                   |
//! |----------------|-----------|------------------------------------------|
//! | `/v0.4/traces` | POST, PUT | trace agent                              |
//! | `/v0.5/traces` | POST, PUT | trace agent                              |
//! | `/v0.6/stats`  | POST, PUT | trace agent                              |
//! | `/info`        | GET       | trace agent                              |
//! | `/flush`       | POST      | this binary's `FlushRouterExtension`     |
//!
//! Environment variables this binary reads:
//!
//! | Variable                        | Purpose                                                                 |
//! |---------------------------------|-------------------------------------------------------------------------|
//! | `DD_APM_DD_URL`                 | Override trace intake URL (parity harness points this at fake-intake)   |
//! | `DD_SITE`                       | Derive stats intake URL when `DD_APM_DD_URL` is unset                   |
//! | `DD_SERVERLESS_FLUSH_STRATEGY`  | Enable periodic flushing (e.g. `periodically,5000`); default = manual   |
//! | `DD_TESTMODE_FUNCTION_ARN`      | Override stub function ARN for tag generation                           |
//! | `DD_LOG_LEVEL`                  | Logging verbosity, parsed by [`bottlecap::config::log_level::LogLevel`] |
//!
//! [APMSVLS-496]: https://datadoghq.atlassian.net/browse/APMSVLS-496

#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(clippy::unwrap_used)]
#![deny(unused_extern_crates)]
#![deny(unused_allocation)]
#![deny(unused_assignments)]
#![deny(unused_comparisons)]
#![deny(unreachable_pub)]
#![deny(missing_copy_implementations)]
#![deny(missing_debug_implementations)]

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

use std::{collections::HashMap, env, path::Path, str::FromStr, sync::Arc};

use axum::{Router, http::StatusCode, routing::post};
use bottlecap::{
    LAMBDA_RUNTIME_SLUG,
    config::{self, flush_strategy::FlushStrategy, log_level::LogLevel},
    flushing::FlushingService,
    lifecycle::{
        flush_control::FlushControl, invocation::processor_service::InvocationProcessorHandle,
    },
    logger,
    logs::{aggregator_service::AggregatorService as LogsAggregatorService, flusher::LogsFlusher},
    startup::build_trace_agent,
    tags::{lambda::tags::FUNCTION_ARN_KEY, provider::Provider as TagProvider},
    traces::trace_agent::RouterExtension,
};
use dogstatsd::{
    aggregator::AggregatorService as MetricsAggregatorService, api_key::ApiKeyFactory,
    constants::CONTEXTS, flusher::Flusher as MetricsFlusher, metric::EMPTY_TAGS,
};
use tokio::signal;
use tracing::error;
use tracing_subscriber::EnvFilter;
use ustr::Ustr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_ustr();
    enable_logging_subsystem();

    // Outside Lambda every `AwsConfig` field falls back via `unwrap_or_default()`,
    // so loading from env is safe with no AWS env vars set. The struct is only
    // read by the secrets resolver, which test-mode bypasses.
    let config = Arc::new(config::get_config(Path::new("")));
    let shared_client = bottlecap::http::get_client(&config);

    // Hardcoded literal API key. The parity harness points at a fake-intake
    // that ignores auth; local dev with a real intake requires a code change.
    let api_key_factory = Arc::new(ApiKeyFactory::new("stub-key"));

    let function_arn = env::var("DD_TESTMODE_FUNCTION_ARN")
        .unwrap_or_else(|_| "arn:aws:lambda:us-east-1:000000000000:function:testmode".to_string());
    let metadata = HashMap::from([(FUNCTION_ARN_KEY.to_string(), function_arn)]);
    let tags_provider = Arc::new(TagProvider::new(
        Arc::clone(&config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &metadata,
    ));

    let invocation_processor_handle = InvocationProcessorHandle::noop();

    // Build the trace pipeline unspawned so we can attach our /flush extension
    // before starting the listener.
    let (trace_agent, pipeline) = build_trace_agent(
        &config,
        &api_key_factory,
        &tags_provider,
        invocation_processor_handle,
        None,
        &shared_client,
    );
    let trace_flusher = Arc::clone(&pipeline.trace_flusher);
    let stats_flusher = Arc::clone(&pipeline.stats_flusher);
    let proxy_flusher = Arc::clone(&pipeline.proxy_flusher);
    let shutdown_token = pipeline.shutdown_token.clone();

    // FlushingService::new takes six non-optional owned values. Test-mode only
    // exercises the trace/stats/proxy flushers; the logs and metrics stubs
    // below stand up real services with empty queues so flushes are no-ops.
    let (logs_aggregator_service, logs_aggregator_handle) = LogsAggregatorService::default();
    tokio::spawn(async move { logs_aggregator_service.run().await });
    let logs_flusher = LogsFlusher::new(
        Arc::clone(&api_key_factory),
        logs_aggregator_handle,
        Arc::clone(&config),
        shared_client.clone(),
    );

    let (metrics_aggregator_service, metrics_aggregator_handle) =
        MetricsAggregatorService::new(EMPTY_TAGS, CONTEXTS).expect("metrics aggregator");
    tokio::spawn(async move { metrics_aggregator_service.run().await });
    let metrics_flushers: Arc<Vec<MetricsFlusher>> = Arc::new(Vec::new());

    let flushing_service = Arc::new(FlushingService::new(
        logs_flusher,
        trace_flusher,
        stats_flusher,
        proxy_flusher,
        metrics_flushers,
        metrics_aggregator_handle,
    ));

    let flush_extension = Arc::new(FlushRouterExtension {
        flushing_service: Arc::clone(&flushing_service),
    });
    let trace_agent = trace_agent.with_router_extension(flush_extension);
    tokio::spawn(async move {
        if let Err(e) = trace_agent.start().await {
            error!("Error starting trace agent: {e:?}");
        }
    });

    // Periodic flush driver. Decoupled from managed-instance mode: any non-Default
    // strategy enables it. Manual flushing via POST /flush always works regardless.
    if config.serverless_flush_strategy != FlushStrategy::Default {
        let mut interval =
            FlushControl::new(config.serverless_flush_strategy, config.flush_timeout)
                .get_flush_interval();
        let fs = Arc::clone(&flushing_service);
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = token.cancelled() => break,
                    _ = interval.tick() => fs.flush_blocking().await,
                }
            }
        });
    }

    signal::ctrl_c().await?;
    // Cancel before the final drain so axum's graceful shutdown drives any
    // in-flight /v0.4/traces requests through the aggregator before the flush
    // reads from it.
    shutdown_token.cancel();
    flushing_service.flush_blocking_final().await;
    Ok(())
}

#[derive(Debug)]
struct FlushRouterExtension {
    flushing_service: Arc<FlushingService>,
}

impl RouterExtension for FlushRouterExtension {
    fn extend(&self, router: Router) -> Result<Router, Box<dyn std::error::Error>> {
        let fs = Arc::clone(&self.flushing_service);
        Ok(router.route(
            "/flush",
            post(move || {
                let fs = Arc::clone(&fs);
                async move {
                    fs.flush_blocking_final().await;
                    StatusCode::NO_CONTENT
                }
            }),
        ))
    }
}

// Warm the ustr pool early so the first SortedTags::parse call (inside
// build_trace_agent and downstream) doesn't pay the 10+ ms init cost.
fn init_ustr() {
    tokio::spawn(async {
        Ustr::from("");
    });
}

fn enable_logging_subsystem() {
    let log_level = LogLevel::from_str(
        std::env::var("DD_LOG_LEVEL")
            .unwrap_or("info".to_string())
            .as_str(),
    )
    .unwrap_or(LogLevel::Info);

    let env_filter = format!(
        "h2=off,hyper=off,reqwest=off,rustls=off,datadog-trace-mini-agent=off,{log_level:?}",
    );
    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(
            EnvFilter::try_new(env_filter).expect("could not parse log level in configuration"),
        )
        .with_level(true)
        .with_thread_names(false)
        .with_thread_ids(false)
        .with_line_number(false)
        .with_file(false)
        .with_target(false)
        .without_time()
        .event_format(logger::Formatter)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
}
