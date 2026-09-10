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
//! | `DD_APM_DD_URL`                 | Override trace intake URL; stats follow it (harness points at fake-intake) |
//! | `DD_SITE`                       | Derive trace and stats intake URLs when `DD_APM_DD_URL` is unset        |
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

use std::{collections::HashMap, env, path::Path, str::FromStr, sync::Arc, time::Duration};

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
    traces::{
        proxy_aggregator,
        trace_agent::{IngestBarrier, RouterExtension},
    },
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

    // Shared proxy aggregator backing the trace agent's proxy endpoints.
    let proxy_aggregator = Arc::new(tokio::sync::Mutex::new(
        proxy_aggregator::Aggregator::default(),
    ));

    // Build the trace pipeline unspawned so we can attach our /flush extension
    // before starting the listener.
    let (trace_agent, pipeline) = build_trace_agent(
        &config,
        &api_key_factory,
        &tags_provider,
        invocation_processor_handle,
        None,
        &shared_client,
        Arc::clone(&proxy_aggregator),
        Some(stats_url_from_trace_intake(&config.apm_dd_url)),
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
        None,
    ));

    let ingest_barrier = trace_agent.ingest_barrier();
    let flush_extension = Arc::new(FlushRouterExtension {
        flushing_service: Arc::clone(&flushing_service),
        ingest_barrier: ingest_barrier.clone(),
    });
    let trace_agent = trace_agent.with_router_extension(flush_extension);
    // Errors are returned rather than logged so that a startup failure (port
    // 8126 already bound, for instance) ends the process instead of leaving a
    // live one with no listener for the harness to connect to.
    let mut listener_task = tokio::spawn(async move {
        trace_agent
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("trace agent failed: {e}"))
    });

    // Periodic flush driver. Decoupled from managed-instance mode: any non-Default
    // strategy enables it. Manual flushing via POST /flush always works regardless.
    if config.ext.serverless_flush_strategy != FlushStrategy::Default {
        let mut interval =
            FlushControl::new(config.ext.serverless_flush_strategy, config.flush_timeout)
                .get_flush_interval();
        let fs = Arc::clone(&flushing_service);
        let token = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    () = token.cancelled() => break,
                    // The periodic driver has no caller to report to; the
                    // flushing service already logs what it dropped.
                    _ = interval.tick() => { fs.flush_blocking().await; },
                }
            }
        });
    }

    // The listener finishing first means it never came up, or stopped serving
    // without being asked to. Either way there is nothing left to drain.
    tokio::select! {
        result = signal::ctrl_c() => result?,
        result = &mut listener_task => match result? {
            Ok(()) => anyhow::bail!("trace agent listener stopped unexpectedly"),
            Err(e) => return Err(e),
        },
    }

    // Cancel before the final drain so axum's graceful shutdown drives any
    // in-flight /v0.4/traces requests through the aggregator before the flush
    // reads from it.
    shutdown_token.cancel();
    // Cancelling only signals the shutdown; awaiting the listener is what
    // guarantees in-flight handlers have finished. Bounded so a lingering
    // connection cannot wedge shutdown: draining late data beats hanging.
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, listener_task).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => error!("Trace agent shut down with an error: {e:?}"),
        Ok(Err(e)) => error!("Trace agent task failed: {e:?}"),
        Err(_) => error!(
            "Trace agent did not shut down within {}s, draining anyway",
            SHUTDOWN_TIMEOUT.as_secs()
        ),
    }
    // Handlers returning does not mean their payloads reached the
    // aggregators; they may still be queued ahead of them.
    ingest_barrier.wait().await;
    flushing_service.flush_blocking_final().await;
    Ok(())
}

/// Path the config crate appends to `DD_APM_DD_URL` to build `apm_dd_url`.
const TRACE_INTAKE_ROUTE: &str = "/api/v0.2/traces";

/// Point stats at the same host as traces.
///
/// `DD_APM_DD_URL` only moves the trace intake; stats would otherwise be
/// derived from `DD_SITE` and leave the harness's fake-intake, so the
/// binary's `/v0.6/stats` path could not be exercised locally. `apm_dd_url`
/// is already a fully-resolved trace endpoint, so strip the trace route
/// before appending the stats one. With `DD_APM_DD_URL` unset this
/// reproduces the site-derived default.
fn stats_url_from_trace_intake(apm_dd_url: &str) -> String {
    libdd_trace_utils::config_utils::trace_stats_url_prefixed(
        apm_dd_url
            .trim_end_matches('/')
            .trim_end_matches(TRACE_INTAKE_ROUTE),
    )
}

#[derive(Debug)]
struct FlushRouterExtension {
    flushing_service: Arc<FlushingService>,
    ingest_barrier: IngestBarrier,
}

/// Upper bound on a single `POST /flush`. The flushers already bound their own
/// HTTP calls via `flush_timeout`, but retries across the five flushers can
/// stack, so this caps total wall-clock time for the harness.
const FLUSH_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on waiting for the HTTP listener to finish its graceful
/// shutdown before the final drain runs.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

impl RouterExtension for FlushRouterExtension {
    fn extend(&self, router: Router) -> Result<Router, Box<dyn std::error::Error + Send + Sync>> {
        let fs = Arc::clone(&self.flushing_service);
        let barrier = self.ingest_barrier.clone();
        Ok(router.route(
            "/flush",
            post(move || {
                let fs = Arc::clone(&fs);
                let barrier = barrier.clone();
                async move {
                    // Isolate panics and bound execution time. flush_blocking_final
                    // expects on the metrics aggregator handle, so a dead aggregator
                    // task would otherwise panic the connection task instead of
                    // returning a status the harness can act on.
                    let mut task = tokio::task::spawn(async move {
                        // A payload can be accepted, and its request answered,
                        // while it is still queued ahead of the aggregators.
                        // Drain those queues first so this flush is
                        // deterministic from the caller's point of view.
                        barrier.wait().await;
                        fs.flush_blocking_final().await
                    });
                    match tokio::time::timeout(FLUSH_REQUEST_TIMEOUT, &mut task).await {
                        Ok(Ok(false)) => StatusCode::NO_CONTENT,
                        // The flush ran, but a flusher gave up on payloads it
                        // could not deliver and they were dropped. Reporting 204
                        // here would tell the harness the drain succeeded.
                        Ok(Ok(true)) => {
                            error!("Flush completed with undelivered payloads");
                            StatusCode::BAD_GATEWAY
                        }
                        Ok(Err(e)) => {
                            error!("Flush task failed: {e:?}");
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                        Err(_) => {
                            task.abort();
                            error!(
                                "Flush timed out after {}s, aborting",
                                FLUSH_REQUEST_TIMEOUT.as_secs()
                            );
                            StatusCode::GATEWAY_TIMEOUT
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_url_follows_the_overridden_trace_intake() {
        assert_eq!(
            stats_url_from_trace_intake("http://127.0.0.1:8080/api/v0.2/traces"),
            "http://127.0.0.1:8080/api/v0.2/stats"
        );
    }

    #[test]
    fn stats_url_matches_the_site_default_when_not_overridden() {
        let site = "datadoghq.com";
        assert_eq!(
            stats_url_from_trace_intake(&libdd_trace_utils::config_utils::trace_intake_url(site)),
            libdd_trace_utils::config_utils::trace_stats_url(site)
        );
    }
}
