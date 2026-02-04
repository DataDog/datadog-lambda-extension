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

use bottlecap::{
    DOGSTATSD_PORT, LAMBDA_RUNTIME_SLUG,
    appsec::processor::{
        Error::FeatureDisabled as AppSecFeatureDisabled, Processor as AppSecProcessor,
    },
    config::{
        self, Config,
        aws::{AwsConfig, build_lambda_function_arn},
        flush_strategy::{FlushStrategy, PeriodicStrategy},
        log_level::LogLevel,
    },
    event_bus::{Event, EventBus},
    extension::{
        self, EXTENSION_HOST, EXTENSION_HOST_IP, ExtensionError, NextEventResponse,
        RegisterResponse,
        telemetry::{
            self, TELEMETRY_PORT,
            events::{TelemetryEvent, TelemetryRecord},
            listener::TelemetryListener,
        },
    },
    fips::{log_fips_status, prepare_client_provider},
    flushing::FlushingService,
    lifecycle::{
        flush_control::{DEFAULT_CONTINUOUS_FLUSH_INTERVAL, FlushControl, FlushDecision},
        invocation::processor_service::{InvocationProcessorHandle, InvocationProcessorService},
        listener::Listener as LifecycleListener,
    },
    logger,
    logs::{
        agent::LogsAgent,
        aggregator_service::{
            AggregatorHandle as LogsAggregatorHandle, AggregatorService as LogsAggregatorService,
        },
        flusher::LogsFlusher,
    },
    otlp::{agent::Agent as OtlpAgent, should_enable_otlp_agent},
    proxy::{interceptor, should_start_proxy},
    secrets::decrypt,
    tags::{
        lambda::{self, tags::EXTENSION_VERSION},
        provider::Provider as TagProvider,
    },
    traces::{
        propagation::DatadogCompositePropagator,
        proxy_aggregator,
        proxy_flusher::Flusher as ProxyFlusher,
        span_dedup_service,
        stats_aggregator::StatsAggregator,
        stats_concentrator_service::{StatsConcentratorHandle, StatsConcentratorService},
        stats_flusher::{self, StatsFlusher},
        stats_generator::StatsGenerator,
        stats_processor, trace_agent,
        trace_aggregator::SendDataBuilderInfo,
        trace_aggregator_service::{
            AggregatorHandle as TraceAggregatorHandle, AggregatorService as TraceAggregatorService,
        },
        trace_flusher::{self, TraceFlusher},
        trace_processor::{self, SendingTraceProcessor},
    },
};
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use decrypt::resolve_secrets;
use dogstatsd::{
    aggregator_service::AggregatorHandle as MetricsAggregatorHandle,
    aggregator_service::AggregatorService as MetricsAggregatorService,
    api_key::ApiKeyFactory,
    constants::CONTEXTS,
    datadog::{
        DdDdUrl, DdUrl, MetricsIntakeUrlPrefix, MetricsIntakeUrlPrefixOverride,
        RetryStrategy as DsdRetryStrategy, Site as MetricsSite,
    },
    dogstatsd::{DogStatsD, DogStatsDConfig},
    flusher::{Flusher as MetricsFlusher, FlusherConfig as MetricsFlusherConfig},
    metric::{EMPTY_TAGS, SortedTags},
};
use libdd_trace_obfuscation::obfuscation_config;
use reqwest::Client;
use std::{collections::hash_map, env, path::Path, str::FromStr, sync::Arc};
use tokio::time::{Duration, Instant};
use tokio::{sync::Mutex as TokioMutex, sync::mpsc::Sender};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};
use tracing_subscriber::EnvFilter;
use ustr::Ustr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let start_time = Instant::now();
    init_ustr();
    enable_logging_subsystem();
    let aws_config = AwsConfig::from_env(start_time);
    log_fips_status(&aws_config.region);
    let version_without_next = EXTENSION_VERSION.split('-').next().unwrap_or("NA");
    debug!("Starting Datadog Extension v{version_without_next}");

    // Debug: Wait for debugger to attach if DD_DEBUG_WAIT_FOR_ATTACH is set
    if let Ok(wait_secs) = env::var("DD_DEBUG_WAIT_FOR_ATTACH") {
        if let Ok(secs) = wait_secs.parse::<u64>() {
            debug!("DD_DEBUG_WAIT_FOR_ATTACH: Waiting {secs} seconds for debugger to attach...");
            debug!("Connect your debugger to port 2345 now!");
            tokio::time::sleep(tokio::time::Duration::from_secs(secs)).await;
            debug!("DD_DEBUG_WAIT_FOR_ATTACH: Continuing execution...");
        }
    }

    prepare_client_provider()?;
    let client = create_reqwest_client_builder()
        .map_err(|e| anyhow::anyhow!("Failed to create client builder: {e:?}"))?
        .no_proxy()
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to create client: {e:?}"))?;

    let cloned_client = client.clone();
    let runtime_api = aws_config.runtime_api.clone();
    let managed_instance_mode = aws_config.is_managed_instance_mode();
    let response = tokio::task::spawn(async move {
        extension::register(
            &cloned_client,
            &runtime_api,
            extension::EXTENSION_NAME,
            managed_instance_mode,
        )
        .await
    });
    // First load the AWS configuration
    let lambda_directory: String =
        env::var("LAMBDA_TASK_ROOT").unwrap_or_else(|_| "/var/task".to_string());
    let config = Arc::new(config::get_config(Path::new(&lambda_directory)));

    let aws_config = Arc::new(aws_config);
    let api_key_factory = create_api_key_factory(&config, &aws_config);

    let r = response
        .await
        .map_err(|e| anyhow::anyhow!("Failed to join task: {e:?}"))?
        .map_err(|e| anyhow::anyhow!("Failed to register extension: {e:?}"))?;

    match extension_loop_active(
        Arc::clone(&aws_config),
        &config,
        &client,
        &r,
        Arc::clone(&api_key_factory),
        start_time,
    )
    .await
    {
        Ok(()) => {
            debug!("Extension loop completed successfully");
            Ok(())
        }
        Err(e) => {
            error!("Extension loop failed: {e:?}, Calling /next without Datadog instrumentation");
            extension_loop_idle(&client, &r, &aws_config).await
        }
    }
}

// Ustr initialization can take 10+ ms.
// Start it early in a separate thread so it won't become a bottleneck later when SortedTags::parse() is called.
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

    debug!("Logging subsystem enabled");
}

/// Returns the appropriate flush strategy for the given mode.
/// In managed instance mode, continuous flush strategy is required for optimal performance.
/// If a different strategy is configured, this function will override it and log an info message.
fn get_flush_strategy_for_mode(
    aws_config: &AwsConfig,
    configured_strategy: FlushStrategy,
) -> FlushStrategy {
    if !aws_config.is_managed_instance_mode() {
        return configured_strategy;
    }

    // Check if flush strategy needs to be enforced and log if so
    if let FlushStrategy::Continuously(_) = configured_strategy {
        configured_strategy
    } else {
        // Only log if the user explicitly configured a non-default strategy
        if !matches!(configured_strategy, FlushStrategy::Default) {
            warn!(
                "Managed Instance mode detected. Flush strategy '{}' is not compatible with managed instance mode. \
                Enforcing continuous flush strategy with {}ms interval for optimal performance.",
                configured_strategy.name(),
                DEFAULT_CONTINUOUS_FLUSH_INTERVAL
            );
        }

        FlushStrategy::Continuously(PeriodicStrategy {
            interval: DEFAULT_CONTINUOUS_FLUSH_INTERVAL,
        })
    }
}

fn create_api_key_factory(config: &Arc<Config>, aws_config: &Arc<AwsConfig>) -> Arc<ApiKeyFactory> {
    let config = Arc::clone(config);
    let aws_config = Arc::clone(aws_config);
    let api_key_secret_reload_interval = config.api_key_secret_reload_interval;

    Arc::new(ApiKeyFactory::new_from_resolver(
        Arc::new(move || {
            let config = Arc::clone(&config);
            let aws_config = Arc::clone(&aws_config);

            Box::pin(async move { resolve_secrets(config, aws_config).await })
        }),
        api_key_secret_reload_interval,
    ))
}

async fn extension_loop_idle(
    client: &Client,
    r: &RegisterResponse,
    aws_config: &AwsConfig,
) -> anyhow::Result<()> {
    loop {
        match extension::next_event(client, &r.extension_id, &aws_config.runtime_api).await {
            Ok(_) => {
                debug!("Extension is idle, skipping next event");
            }
            Err(e) => {
                error!("Error getting next event: {e:?}");
                return Err(e.into());
            }
        };
    }
}

#[allow(clippy::too_many_lines)]
async fn extension_loop_active(
    aws_config: Arc<AwsConfig>,
    config: &Arc<Config>,
    client: &Client,
    r: &RegisterResponse,
    api_key_factory: Arc<ApiKeyFactory>,
    start_time: Instant,
) -> anyhow::Result<()> {
    let (mut event_bus, event_bus_tx) = EventBus::run();

    let account_id = r
        .account_id
        .as_ref()
        .unwrap_or(&"none".to_string())
        .to_string();
    let tags_provider = setup_tag_provider(&Arc::clone(&aws_config), config, &account_id);

    let (logs_agent_channel, logs_flusher, logs_agent_cancel_token, logs_aggregator_handle) =
        start_logs_agent(
            config,
            Arc::clone(&api_key_factory),
            &tags_provider,
            event_bus_tx.clone(),
            aws_config.is_managed_instance_mode(),
        );

    let (metrics_flushers, metrics_aggregator_handle, dogstatsd_cancel_token) =
        start_dogstatsd(tags_provider.clone(), Arc::clone(&api_key_factory), config).await;

    let propagator = Arc::new(DatadogCompositePropagator::new(Arc::clone(config)));
    // Lifecycle Invocation Processor
    let (invocation_processor_handle, invocation_processor_service) =
        InvocationProcessorService::new(
            Arc::clone(&tags_provider),
            Arc::clone(config),
            Arc::clone(&aws_config),
            metrics_aggregator_handle.clone(),
            Arc::clone(&propagator),
        );
    tokio::spawn(async move {
        invocation_processor_service.run().await;
    });

    // AppSec processor (if enabled)
    let appsec_processor = match AppSecProcessor::new(config) {
        Ok(p) => Some(Arc::new(TokioMutex::new(p))),
        Err(AppSecFeatureDisabled) => None,
        Err(e) => {
            error!(
                "AAP | error creating App & API Protection processor, the feature will be disabled: {e}"
            );
            None
        }
    };

    let (
        trace_agent_channel,
        trace_flusher,
        trace_processor,
        stats_flusher,
        proxy_flusher,
        trace_agent_shutdown_token,
        stats_concentrator,
        trace_aggregator_handle,
    ) = start_trace_agent(
        config,
        &api_key_factory,
        &tags_provider,
        invocation_processor_handle.clone(),
        appsec_processor.clone(),
    );

    let api_runtime_proxy_shutdown_signal = start_api_runtime_proxy(
        config,
        Arc::clone(&aws_config),
        &invocation_processor_handle,
        appsec_processor.as_ref(),
        Arc::clone(&propagator),
    );

    let lifecycle_listener =
        LifecycleListener::new(invocation_processor_handle.clone(), Arc::clone(&propagator));
    let lifecycle_listener_shutdown_token = lifecycle_listener.get_shutdown_token();
    // TODO(astuyve): deprioritize this task after the first request
    tokio::spawn(async move {
        if let Err(e) = lifecycle_listener.start().await {
            error!("Error starting lifecycle listener: {e:?}");
        }
    });

    let telemetry_listener_cancel_token = setup_telemetry_client(
        client,
        &r.extension_id,
        &aws_config.runtime_api,
        logs_agent_channel,
        event_bus_tx.clone(),
        config.serverless_logs_enabled,
        aws_config.is_managed_instance_mode(),
    )
    .await?;

    let otlp_cancel_token = start_otlp_agent(
        config,
        tags_provider.clone(),
        trace_processor.clone(),
        trace_agent_channel.clone(),
        stats_concentrator.clone(),
    );

    // Validate and get the appropriate flush strategy for the current mode
    let flush_strategy = get_flush_strategy_for_mode(&aws_config, config.serverless_flush_strategy);
    debug!("Flush strategy: {:?}", flush_strategy);
    let mut flush_control = FlushControl::new(flush_strategy, config.flush_timeout);

    debug!(
        "Datadog Next-Gen Extension ready in {:}ms",
        start_time.elapsed().as_millis().to_string()
    );

    if aws_config.is_managed_instance_mode() {
        // Clone Arc references for the background flusher task
        let logs_flusher_clone = logs_flusher.clone();
        let metrics_flushers_clone = Arc::clone(&metrics_flushers);
        let trace_flusher_clone = Arc::clone(&trace_flusher);
        let stats_flusher_clone = Arc::clone(&stats_flusher);
        let proxy_flusher_clone = proxy_flusher.clone();
        let metrics_aggr_handle_clone = metrics_aggregator_handle.clone();

        // In Managed Instance mode, create a separate interval for the background flusher task.
        // We don't reuse race_flush_interval because we need to configure the missed tick
        // behavior before discarding the first tick. While creating a new interval may seem
        // redundant, it keeps Managed Instance and OnDemand mode flush intervals properly isolated,
        // making the code easier to maintain and less error-prone.
        //
        // Use Skip behavior to prevent accumulating missed ticks if flushes take longer
        // than the interval. This ensures we maintain a steady flush cadence without
        // bursts of catch-up ticks, which is important since flushes are non-blocking.
        let mut managed_instance_mode_flush_interval = flush_control.get_flush_interval();
        managed_instance_mode_flush_interval
            .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        managed_instance_mode_flush_interval.tick().await; // discard first tick
        let cancel_token_clone = telemetry_listener_cancel_token.clone();

        // Spawn a background task for continuous periodic flushing in Managed Instance mode.
        // A background task continuously flushes metrics, logs,
        // traces, and stats at regular intervals (configured by flush_control). This ensures
        // data is sent to Datadog even while concurrent invocations are being processed.
        // The flushing happens independently of invocation lifecycle events.
        // This background task runs until shutdown is signaled via cancel_token_clone.
        let flush_task_handle = tokio::spawn(async move {
            let mut flushing_service = FlushingService::new(
                logs_flusher_clone,
                trace_flusher_clone,
                stats_flusher_clone,
                proxy_flusher_clone,
                metrics_flushers_clone,
                metrics_aggr_handle_clone,
            );

            loop {
                tokio::select! {
                    _ = managed_instance_mode_flush_interval.tick() => {
                        if !flushing_service.has_pending_handles() {
                            // Only spawn new flush if no pending flushes to prevent resource buildup
                            flushing_service.spawn_non_blocking().await;
                        }
                    }
                   () = cancel_token_clone.cancelled() => {
                        debug!("Managed Instance mode: periodic flusher task cancelled, waiting for pending flushes");
                        // Wait for any pending flushes before exiting
                        flushing_service.await_handles().await;
                        break;
                    }
                }
            }
        });

        // Spawn a separate task to handle the SHUTDOWN event from /next endpoint.
        // This task waits for the Lambda runtime to signal shutdown via the Extensions API.
        // When shutdown is received, it cancels the background flusher and signals the main
        // event loop to begin graceful shutdown.
        let shutdown_cancel_token = CancellationToken::new();
        let shutdown_cancel_token_clone = shutdown_cancel_token.clone();
        let invocation_processor_handle_clone = invocation_processor_handle.clone();
        let runtime_api_clone = aws_config.runtime_api.clone();
        let extension_id_clone = r.extension_id.clone();
        let client_clone = client.clone();

        // Main event loop for Managed Instance mode: process telemetry events until shutdown
        //
        // the extension registers for SHUTDOWN events ONLY (not INVOKE events),
        //   1. A separate task waits for the SHUTDOWN event from next_event()
        //   2. This loop processes telemetry events from event_bus
        //   3. When SHUTDOWN is received (detected via cancel token), break the loop
        //   4. Invocation lifecycle events (START, REPORT, etc.) come via Telemetry API,
        //      not via next_event() responses
        //
        // This allows Managed Instance mode to handle concurrent invocations while OnDemand mode
        // processes invocations sequentially, one at a time.
        tokio::spawn(async move {
            // In Managed Instance mode, the only event we can subscribe to is SHUTDOWN, meaning that
            // this call will block until the shutdown event is received.
            // We can use this to signal other tasks to shutdown and wait for them to complete.
            // Therefore, we need to have it in a separate task to avoid blocking the main loop.
            debug!("Managed Instance mode: waiting for shutdown event");

            loop {
                let next_response =
                    extension::next_event(&client_clone, &runtime_api_clone, &extension_id_clone)
                        .await;

                match next_response {
                    Ok(NextEventResponse::Shutdown { .. }) => {
                        debug!("Shutdown event received, stopping extension loop");
                        // Notify the invocation processor about shutdown
                        if let Err(e) = invocation_processor_handle_clone.on_shutdown_event().await
                        {
                            error!("Failed to send shutdown event to processor: {}", e);
                        }
                        // Signal all other tasks to shutdown
                        shutdown_cancel_token_clone.cancel();
                        break;
                    }
                    Ok(NextEventResponse::Invoke { .. }) => {
                        error!(
                            "Received unexpected Invoke event in Managed Instance mode - this should not happen. \
                            Managed Instance mode should only subscribe to SHUTDOWN events."
                        );
                        shutdown_cancel_token_clone.cancel();
                        break;
                    }
                    Err(ExtensionError::HttpError(e)) if e.is_timeout() || e.is_connect() => {
                        debug!(
                            "Transient network error waiting for shutdown event: {}. Retrying...",
                            e
                        );
                        continue;
                    }
                    Err(e) => {
                        error!(
                            "Unrecoverable error waiting for shutdown event: {:?}. \
                            Initiating emergency shutdown.",
                            e
                        );
                        shutdown_cancel_token_clone.cancel();
                        break;
                    }
                }
            }

            debug!("Shutdown task completed");
        });

        'managed_instance_event_loop: loop {
            tokio::select! {
              biased;
                // Process telemetry events (platform.start, platform.report, etc.) sent from
                // the Telemetry API listener. These events provide invocation lifecycle information
                // without requiring next_event() calls. The biased ordering ensures we prioritize
                // processing telemetry events before checking for shutdown.
                Some(event) = event_bus.rx.recv() => {
                      handle_event_bus_event(event,
                          invocation_processor_handle.clone(),
                          appsec_processor.clone(),
                          tags_provider.clone(),
                          trace_processor.clone(),
                          trace_agent_channel.clone(),
                        stats_concentrator.clone()).
                      await;
                    }
                // Detect when shutdown has been signaled by the shutdown task.
                // This happens when the /next endpoint returns a SHUTDOWN event.
                () = shutdown_cancel_token.cancelled() => {
                    debug!("Shutdown signal received, exiting event loop");
                    break 'managed_instance_event_loop;
                }
            }
        }

        // Shutdown sequence
        debug!("Initiating shutdown sequence");

        // Wait for tombstone event from telemetry listener to ensure all events are processed
        // This is the result of code refactoring which is shared by OnDemand mode as well.
        wait_for_tombstone_event(
            &mut event_bus,
            &invocation_processor_handle,
            appsec_processor.clone(),
            tags_provider.clone(),
            trace_processor.clone(),
            trace_agent_channel.clone(),
            stats_concentrator.clone(),
            300,
        )
        .await;

        // Cancel background tasks
        cancel_background_services(
            api_runtime_proxy_shutdown_signal.as_ref(),
            otlp_cancel_token.as_ref(),
            &trace_agent_shutdown_token,
            &dogstatsd_cancel_token,
            &telemetry_listener_cancel_token,
            &lifecycle_listener_shutdown_token,
        );

        // Wait for background flusher to complete gracefully
        if let Err(e) = flush_task_handle.await {
            error!("Error waiting for background flush task: {e:?}");
        }

        // Final flush to send any remaining observability data before shutdown.
        //
        // Managed Instance Mode vs OnDemand Mode Final Flush:
        //
        // While both modes perform a final flush during shutdown, the context differs:
        //
        // - **Managed Instance Mode (this code)**: Throughout the execution environment's lifetime,
        //   a background task has been continuously flushing data at regular intervals
        //   (see flush_task_handle above). This final flush captures any data that was
        //   generated after the last periodic flush and before shutdown was signaled.
        //   Since concurrent invocations may have completed just before shutdown, this
        //   ensures we don't lose their metrics, logs, and traces.
        //
        // - **OnDemand Mode**: Flushing is tied to invocation lifecycle, so data is typically
        //   flushed at the end of each invocation. The final flush captures any remaining
        //   data from the last invocation that may not have been sent yet.
        //
        // In both modes, we pass `force_flush_trace_stats=true` to ensure trace statistics
        // are flushed regardless of timing constraints, as this is our last opportunity to
        // send data before the Lambda execution environment terminates.
        //
        // Final flush without interval reset. We pass None for race_flush_interval since
        // this is the final operation before shutdown and resetting the interval timing
        // serves no purpose. This avoids creating an unnecessary interval object.
        let flushing_service = FlushingService::new(
            logs_flusher.clone(),
            Arc::clone(&trace_flusher),
            Arc::clone(&stats_flusher),
            proxy_flusher.clone(),
            Arc::clone(&metrics_flushers),
            metrics_aggregator_handle.clone(),
        );
        let mut locked_metrics = metrics_flushers.lock().await;
        flushing_service
            .flush_blocking(true, &mut locked_metrics)
            .await;

        return Ok(());
    }

    // Below is for On-Demand mode only
    let mut race_flush_interval = flush_control.get_flush_interval();
    race_flush_interval.tick().await; // discard first tick, which is instantaneous

    let next_lambda_response =
        extension::next_event(client, &aws_config.runtime_api, &r.extension_id).await;
    // first invoke we must call next
    let mut flushing_service = FlushingService::new(
        logs_flusher.clone(),
        Arc::clone(&trace_flusher),
        Arc::clone(&stats_flusher),
        proxy_flusher.clone(),
        Arc::clone(&metrics_flushers),
        metrics_aggregator_handle.clone(),
    );
    handle_next_invocation(next_lambda_response, &invocation_processor_handle).await;
    loop {
        let maybe_shutdown_event;

        let current_flush_decision = flush_control.evaluate_flush_decision();
        match current_flush_decision {
            FlushDecision::End => {
                // break loop after runtime done
                // flush everything
                // call next
                // optionally flush after tick for long running invos
                'flush_end: loop {
                    tokio::select! {
                    biased;
                        Some(event) = event_bus.rx.recv() => {
                            if let Some(telemetry_event) = handle_event_bus_event(event, invocation_processor_handle.clone(), appsec_processor.clone(), tags_provider.clone(), trace_processor.clone(), trace_agent_channel.clone(), stats_concentrator.clone()).await {
                                if let TelemetryRecord::PlatformRuntimeDone{ .. } = telemetry_event.record {
                                    break 'flush_end;
                                }
                            }
                        }
                        _ = race_flush_interval.tick() => {
                            let mut locked_metrics = metrics_flushers.lock().await;
                            flushing_service
                                .flush_blocking(false, &mut locked_metrics)
                                .await;
                            race_flush_interval.reset();
                        }
                    }
                }
                // flush
                let mut locked_metrics = metrics_flushers.lock().await;
                flushing_service
                    .flush_blocking(false, &mut locked_metrics)
                    .await;
                race_flush_interval.reset();
                let next_response =
                    extension::next_event(client, &aws_config.runtime_api, &r.extension_id).await;
                maybe_shutdown_event =
                    handle_next_invocation(next_response, &invocation_processor_handle).await;
            }
            FlushDecision::Continuous | FlushDecision::Periodic | FlushDecision::Dont => {
                match current_flush_decision {
                    //Periodic flush scenario, flush at top of invocation
                    FlushDecision::Continuous => {
                        if !flushing_service.has_pending_handles() {
                            flushing_service.spawn_non_blocking().await;
                            race_flush_interval.reset();
                        }
                    }
                    FlushDecision::Periodic => {
                        let mut locked_metrics = metrics_flushers.lock().await;
                        flushing_service
                            .flush_blocking(false, &mut locked_metrics)
                            .await;
                        race_flush_interval.reset();
                    }
                    _ => {
                        // No specific flush logic for Dont or End (End already handled above)
                    }
                }
                // NO FLUSH SCENARIO
                // JUST LOOP OVER PIPELINE AND WAIT FOR NEXT EVENT
                // If we get platform.runtimeDone or platform.runtimeReport
                // That's fine, we still wait to break until we get the response from next
                // and then we break to determine if we'll flush or not
                let next_lambda_response =
                    extension::next_event(client, &aws_config.runtime_api, &r.extension_id);
                tokio::pin!(next_lambda_response);
                'next_invocation: loop {
                    tokio::select! {
                    biased;
                        next_response = &mut next_lambda_response => {
                            maybe_shutdown_event = handle_next_invocation(next_response, &invocation_processor_handle).await;
                            // Need to break here to re-call next
                            break 'next_invocation;
                        }
                        Some(event) = event_bus.rx.recv() => {
                            handle_event_bus_event(event, invocation_processor_handle.clone(), appsec_processor.clone(), tags_provider.clone(), trace_processor.clone(), trace_agent_channel.clone(), stats_concentrator.clone()).await;
                        }
                        _ = race_flush_interval.tick() => {
                            if flush_control.flush_strategy == FlushStrategy::Default {
                                let mut locked_metrics = metrics_flushers.lock().await;
                                flushing_service
                                    .flush_blocking(false, &mut locked_metrics)
                                    .await;
                                race_flush_interval.reset();
                            }
                        }
                    }
                }
            }
        }

        if let NextEventResponse::Shutdown { .. } = maybe_shutdown_event {
            // Cancel Telemetry API listener
            // Important to do this first, so we can receive the Tombstone event which signals
            // that there are no more Telemetry events to process
            telemetry_listener_cancel_token.cancel();

            // Cancel Logs Agent which might have Telemetry API events to process
            logs_agent_cancel_token.cancel();

            // Drop the event bus sender to allow the channel to close properly
            drop(event_bus_tx);

            // Redrive/block on any failed payloads
            flushing_service.await_handles().await;
            // Wait for tombstone event from telemetry listener to ensure all events are processed
            wait_for_tombstone_event(
                &mut event_bus,
                &invocation_processor_handle,
                appsec_processor.clone(),
                tags_provider.clone(),
                trace_processor.clone(),
                trace_agent_channel.clone(),
                stats_concentrator.clone(),
                300,
            )
            .await;

            // Cancel background services
            cancel_background_services(
                api_runtime_proxy_shutdown_signal.as_ref(),
                otlp_cancel_token.as_ref(),
                &trace_agent_shutdown_token,
                &dogstatsd_cancel_token,
                &telemetry_listener_cancel_token,
                &lifecycle_listener_shutdown_token,
            );

            // Final flush with force_stats=true since this is our last opportunity
            let mut locked_metrics = metrics_flushers.lock().await;
            flushing_service
                .flush_blocking(true, &mut locked_metrics)
                .await;

            // Shutdown aggregator services
            if let Err(e) = logs_aggregator_handle.shutdown() {
                error!("Failed to shutdown logs aggregator: {e}");
            }
            if let Err(e) = trace_aggregator_handle.shutdown() {
                error!("Failed to shutdown trace aggregator: {e}");
            }

            return Ok(());
        }
    }
}

/// Wait for the `Tombstone` event from telemetry listener to ensure all events are processed.
/// This function will timeout after the specified duration to prevent hanging indefinitely.
#[allow(clippy::too_many_arguments)]
async fn wait_for_tombstone_event(
    event_bus: &mut EventBus,
    invocation_processor_handle: &InvocationProcessorHandle,
    appsec_processor: Option<Arc<TokioMutex<AppSecProcessor>>>,
    tags_provider: Arc<TagProvider>,
    trace_processor: Arc<trace_processor::ServerlessTraceProcessor>,
    trace_agent_channel: Sender<SendDataBuilderInfo>,
    stats_concentrator: StatsConcentratorHandle,
    timeout_ms: u64,
) {
    'shutdown: loop {
        tokio::select! {
            Some(event) = event_bus.rx.recv() => {
                if let Event::Tombstone = event {
                    debug!("Received tombstone event, proceeding with shutdown");
                    break 'shutdown;
                }
                handle_event_bus_event(
                    event,
                    invocation_processor_handle.clone(),
                    appsec_processor.clone(),
                    tags_provider.clone(),
                    trace_processor.clone(),
                    trace_agent_channel.clone(),
                    stats_concentrator.clone(),
                ).await;
            }
            () = tokio::time::sleep(tokio::time::Duration::from_millis(timeout_ms)) => {
                debug!("Timeout waiting for tombstone event, proceeding with shutdown");
                break 'shutdown;
            }
        }
    }
}

/// Cancel all background service tasks in preparation for shutdown.
fn cancel_background_services(
    api_runtime_proxy_shutdown_signal: Option<&CancellationToken>,
    otlp_cancel_token: Option<&CancellationToken>,
    trace_agent_shutdown_token: &CancellationToken,
    dogstatsd_cancel_token: &CancellationToken,
    telemetry_listener_cancel_token: &CancellationToken,
    lifecycle_listener_shutdown_token: &CancellationToken,
) {
    if let Some(token) = api_runtime_proxy_shutdown_signal {
        token.cancel();
    }
    if let Some(token) = otlp_cancel_token {
        token.cancel();
    }
    trace_agent_shutdown_token.cancel();
    dogstatsd_cancel_token.cancel();
    telemetry_listener_cancel_token.cancel();
    lifecycle_listener_shutdown_token.cancel();
}

#[allow(clippy::too_many_lines)]
async fn handle_event_bus_event(
    event: Event,
    invocation_processor_handle: InvocationProcessorHandle,
    appsec_processor: Option<Arc<TokioMutex<AppSecProcessor>>>,
    tags_provider: Arc<TagProvider>,
    trace_processor: Arc<trace_processor::ServerlessTraceProcessor>,
    trace_agent_channel: Sender<SendDataBuilderInfo>,
    stats_concentrator: StatsConcentratorHandle,
) -> Option<TelemetryEvent> {
    match event {
        Event::OutOfMemory(event_timestamp) => {
            if let Err(e) = invocation_processor_handle
                .on_out_of_memory_error(event_timestamp)
                .await
            {
                error!("Failed to send out of memory error to processor: {}", e);
            }
        }
        Event::Telemetry(event) => {
            debug!("Telemetry event received: {:?}", event);
            match event.record {
                TelemetryRecord::PlatformInitStart { .. } => {
                    if let Err(e) = invocation_processor_handle
                        .on_platform_init_start(event.time)
                        .await
                    {
                        error!("Failed to send platform init start to processor: {}", e);
                    }
                }
                TelemetryRecord::PlatformInitReport {
                    metrics,
                    initialization_type,
                    ..
                } => {
                    if let Err(e) = invocation_processor_handle
                        .on_platform_init_report(
                            initialization_type,
                            metrics.duration_ms,
                            event.time.timestamp(),
                        )
                        .await
                    {
                        error!("Failed to send platform init report to processor: {}", e);
                    }
                }
                TelemetryRecord::PlatformRestoreStart { .. } => {
                    if let Err(e) = invocation_processor_handle
                        .on_platform_restore_start(event.time)
                        .await
                    {
                        error!("Failed to send platform restore start to processor: {}", e);
                    }
                }
                TelemetryRecord::PlatformRestoreReport { metrics, .. } => {
                    if let Some(m) = metrics {
                        if let Err(e) = invocation_processor_handle
                            .on_platform_restore_report(m.duration_ms, event.time.timestamp())
                            .await
                        {
                            error!("Failed to send platform restore report to processor: {}", e);
                        }
                    } else {
                        error!(
                            "Missing SnapStart RestoreReportMetric. Not creating SnapStart span."
                        );
                    }
                }
                TelemetryRecord::PlatformStart { request_id, .. } => {
                    if let Err(e) = invocation_processor_handle
                        .on_platform_start(request_id, event.time)
                        .await
                    {
                        error!("Failed to send platform start to processor: {}", e);
                    }
                }
                TelemetryRecord::PlatformRuntimeDone {
                    ref request_id,
                    metrics: Some(metrics),
                    status,
                    ref error_type,
                    ..
                } => {
                    if let Err(e) = invocation_processor_handle
                        .on_platform_runtime_done(
                            request_id.clone(),
                            metrics,
                            status,
                            error_type.clone(),
                            tags_provider.clone(),
                            Arc::new(SendingTraceProcessor {
                                appsec: appsec_processor.clone(),
                                processor: trace_processor.clone(),
                                trace_tx: trace_agent_channel.clone(),
                                stats_generator: Arc::new(StatsGenerator::new(
                                    stats_concentrator.clone(),
                                )),
                            }),
                            event.time.timestamp(),
                        )
                        .await
                    {
                        error!("Failed to send platform runtime done to processor: {}", e);
                    }
                    return Some(event);
                }
                TelemetryRecord::PlatformReport {
                    ref request_id,
                    metrics,
                    status,
                    ref error_type,
                    ref spans,
                } => {
                    if let Err(e) = invocation_processor_handle
                        .on_platform_report(
                            request_id,
                            metrics,
                            event.time.timestamp(),
                            status,
                            error_type,
                            spans,
                            tags_provider.clone(),
                            Arc::new(SendingTraceProcessor {
                                appsec: appsec_processor.clone(),
                                processor: trace_processor.clone(),
                                trace_tx: trace_agent_channel.clone(),
                                stats_generator: Arc::new(StatsGenerator::new(
                                    stats_concentrator.clone(),
                                )),
                            }),
                        )
                        .await
                    {
                        error!("Failed to send platform runtime report to processor: {}", e);
                    }
                    return Some(event);
                }
                _ => {
                    debug!("Unforwarded Telemetry event: {:?}", event);
                }
            }
        }
        // Nothing to do with Tombstone event
        Event::Tombstone => {}
    }
    None
}

async fn handle_next_invocation(
    next_response: Result<NextEventResponse, ExtensionError>,
    invocation_processor_handle: &InvocationProcessorHandle,
) -> NextEventResponse {
    match next_response {
        Ok(NextEventResponse::Invoke {
            ref request_id,
            deadline_ms,
            ref invoked_function_arn,
        }) => {
            debug!(
                "Invoke event {}; deadline: {}, invoked_function_arn: {}",
                request_id.clone(),
                deadline_ms,
                invoked_function_arn.clone()
            );
            if let Err(e) = invocation_processor_handle
                .on_invoke_event(request_id.into())
                .await
            {
                error!("Failed to send invoke event to processor: {}", e);
            }
        }
        Ok(NextEventResponse::Shutdown {
            ref shutdown_reason,
            deadline_ms,
        }) => {
            if let Err(e) = invocation_processor_handle.on_shutdown_event().await {
                error!("Failed to send shutdown event to processor: {}", e);
            }
            println!("Exiting: {shutdown_reason}, deadline: {deadline_ms}");
        }
        Err(ref err) => {
            eprintln!("Error: {err:?}");
            println!("Exiting");
        }
    }
    next_response.unwrap_or(NextEventResponse::Shutdown {
        shutdown_reason: "panic".into(),
        deadline_ms: 0,
    })
}

fn setup_tag_provider(
    aws_config: &Arc<AwsConfig>,
    config: &Arc<Config>,
    account_id: &str,
) -> Arc<TagProvider> {
    let function_arn =
        build_lambda_function_arn(account_id, &aws_config.region, &aws_config.function_name);
    let metadata_hash = hash_map::HashMap::from([(
        lambda::tags::FUNCTION_ARN_KEY.to_string(),
        function_arn.clone(),
    )]);
    Arc::new(TagProvider::new(
        Arc::clone(config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &metadata_hash,
    ))
}

fn start_logs_agent(
    config: &Arc<Config>,
    api_key_factory: Arc<ApiKeyFactory>,
    tags_provider: &Arc<TagProvider>,
    event_bus: Sender<Event>,
    is_managed_instance_mode: bool,
) -> (
    Sender<TelemetryEvent>,
    LogsFlusher,
    CancellationToken,
    LogsAggregatorHandle,
) {
    let (aggregator_service, aggregator_handle) = LogsAggregatorService::default();
    // Start service in background
    tokio::spawn(async move {
        aggregator_service.run().await;
    });

    let (mut agent, tx) = LogsAgent::new(
        Arc::clone(tags_provider),
        Arc::clone(config),
        event_bus,
        aggregator_handle.clone(),
        is_managed_instance_mode,
    );
    let cancel_token = agent.cancel_token();
    // Start logs agent in background
    tokio::spawn(async move {
        agent.spin().await;

        debug!("LOGS_AGENT | Shutting down...");
        drop(agent);
    });

    let flusher = LogsFlusher::new(api_key_factory, aggregator_handle.clone(), config.clone());
    (tx, flusher, cancel_token, aggregator_handle)
}

#[allow(clippy::type_complexity)]
fn start_trace_agent(
    config: &Arc<Config>,
    api_key_factory: &Arc<ApiKeyFactory>,
    tags_provider: &Arc<TagProvider>,
    invocation_processor_handle: InvocationProcessorHandle,
    appsec_processor: Option<Arc<TokioMutex<AppSecProcessor>>>,
) -> (
    Sender<SendDataBuilderInfo>,
    Arc<trace_flusher::ServerlessTraceFlusher>,
    Arc<trace_processor::ServerlessTraceProcessor>,
    Arc<stats_flusher::ServerlessStatsFlusher>,
    Arc<ProxyFlusher>,
    tokio_util::sync::CancellationToken,
    StatsConcentratorHandle,
    TraceAggregatorHandle,
) {
    // Stats
    let (stats_concentrator_service, stats_concentrator_handle) =
        StatsConcentratorService::new(Arc::clone(config));
    tokio::spawn(stats_concentrator_service.run());
    let stats_aggregator: Arc<TokioMutex<StatsAggregator>> = Arc::new(TokioMutex::new(
        StatsAggregator::new_with_concentrator(stats_concentrator_handle.clone()),
    ));
    let stats_flusher = Arc::new(stats_flusher::ServerlessStatsFlusher::new(
        api_key_factory.clone(),
        stats_aggregator.clone(),
        Arc::clone(config),
    ));

    let stats_processor = Arc::new(stats_processor::ServerlessStatsProcessor {});

    // Traces
    let (trace_aggregator_service, trace_aggregator_handle) = TraceAggregatorService::default();
    tokio::spawn(trace_aggregator_service.run());

    let trace_flusher = Arc::new(trace_flusher::ServerlessTraceFlusher::new(
        trace_aggregator_handle.clone(),
        config.clone(),
        api_key_factory.clone(),
    ));

    let obfuscation_config = obfuscation_config::ObfuscationConfig {
        tag_replace_rules: config.apm_replace_tags.clone(),
        http_remove_path_digits: config.apm_config_obfuscation_http_remove_paths_with_digits,
        http_remove_query_string: config.apm_config_obfuscation_http_remove_query_string,
        obfuscate_memcached: false,
        obfuscation_redis_enabled: false,
        obfuscation_redis_remove_all_args: false,
    };

    let trace_processor = Arc::new(trace_processor::ServerlessTraceProcessor {
        obfuscation_config: Arc::new(obfuscation_config),
    });

    let (span_dedup_service, span_dedup_handle) = span_dedup_service::DedupService::new();
    tokio::spawn(span_dedup_service.run());

    // Proxy
    let proxy_aggregator = Arc::new(TokioMutex::new(proxy_aggregator::Aggregator::default()));
    let proxy_flusher = Arc::new(ProxyFlusher::new(
        api_key_factory.clone(),
        Arc::clone(&proxy_aggregator),
        Arc::clone(tags_provider),
        Arc::clone(config),
    ));

    let trace_agent = trace_agent::TraceAgent::new(
        Arc::clone(config),
        trace_aggregator_handle.clone(),
        trace_processor.clone(),
        stats_aggregator,
        stats_processor,
        proxy_aggregator,
        invocation_processor_handle,
        appsec_processor,
        Arc::clone(tags_provider),
        stats_concentrator_handle.clone(),
        span_dedup_handle,
    );
    let trace_agent_channel = trace_agent.get_sender_copy();
    let shutdown_token = trace_agent.shutdown_token();

    tokio::spawn(async move {
        if let Err(e) = trace_agent.start().await {
            error!("Error starting trace agent: {e:?}");
        }
    });

    (
        trace_agent_channel,
        trace_flusher,
        trace_processor,
        stats_flusher,
        proxy_flusher,
        shutdown_token,
        stats_concentrator_handle,
        trace_aggregator_handle,
    )
}

async fn start_dogstatsd(
    tags_provider: Arc<TagProvider>,
    api_key_factory: Arc<ApiKeyFactory>,
    config: &Arc<Config>,
) -> (
    Arc<TokioMutex<Vec<MetricsFlusher>>>,
    MetricsAggregatorHandle,
    CancellationToken,
) {
    // Start aggregator service and handle
    let start_time = Instant::now();
    let (aggregator_service, aggregator_handle) = MetricsAggregatorService::new(
        SortedTags::parse(&tags_provider.get_tags_string()).unwrap_or(EMPTY_TAGS),
        CONTEXTS,
    )
    .expect("can't create metrics service");
    debug!(
        "Metrics aggregator created in {:} microseconds",
        start_time.elapsed().as_micros().to_string()
    );

    // Start service in background
    tokio::spawn(async move {
        aggregator_service.run().await;
    });

    // Get flushers with aggregator handle
    let flushers = Arc::new(TokioMutex::new(start_metrics_flushers(
        Arc::clone(&api_key_factory),
        &aggregator_handle,
        config,
    )));

    // Create Dogstatsd server
    let dogstatsd_config = DogStatsDConfig {
        host: EXTENSION_HOST.to_string(),
        port: DOGSTATSD_PORT,
        metric_namespace: config.statsd_metric_namespace.clone(),
    };
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let dogstatsd_agent = DogStatsD::new(
        &dogstatsd_config,
        aggregator_handle.clone(),
        cancel_token.clone(),
    )
    .await;

    // Start server in background
    tokio::spawn(async move {
        dogstatsd_agent.spin().await;
    });

    (flushers, aggregator_handle, cancel_token)
}

fn start_metrics_flushers(
    api_key_factory: Arc<ApiKeyFactory>,
    metrics_aggr_handle: &MetricsAggregatorHandle,
    config: &Arc<Config>,
) -> Vec<MetricsFlusher> {
    let mut flushers = Vec::new();

    let metrics_intake_url = if !config.dd_url.is_empty() {
        let dd_dd_url = DdDdUrl::new(config.dd_url.clone()).expect("can't parse DD_DD_URL");

        let prefix_override = MetricsIntakeUrlPrefixOverride::maybe_new(None, Some(dd_dd_url));
        MetricsIntakeUrlPrefix::new(None, prefix_override)
    } else if !config.url.is_empty() {
        let dd_url = DdUrl::new(config.url.clone()).expect("can't parse DD_URL");

        let prefix_override = MetricsIntakeUrlPrefixOverride::maybe_new(Some(dd_url), None);
        MetricsIntakeUrlPrefix::new(None, prefix_override)
    } else {
        // use site
        let metrics_site = MetricsSite::new(config.site.clone()).expect("can't parse site");
        MetricsIntakeUrlPrefix::new(Some(metrics_site), None)
    };

    let flusher_config = MetricsFlusherConfig {
        api_key_factory,
        aggregator_handle: metrics_aggr_handle.clone(),
        metrics_intake_url_prefix: metrics_intake_url.expect("can't parse site or override"),
        https_proxy: config.proxy_https.clone(),
        ca_cert_path: config.tls_cert_file.clone(),
        timeout: Duration::from_secs(config.flush_timeout),
        retry_strategy: DsdRetryStrategy::Immediate(3),
        compression_level: config.metrics_config_compression_level,
    };
    flushers.push(MetricsFlusher::new(flusher_config));

    for (endpoint_url, api_keys) in &config.additional_endpoints {
        let dd_url = match DdUrl::new(endpoint_url.clone()) {
            Ok(url) => url,
            Err(err) => {
                error!(
                    "Invalid additional endpoint: {err}. Falling back to 'https://app.datadoghq.com'"
                );
                DdUrl::new("https://app.datadoghq.com".to_string())
                    .expect("additional endpoint fallback URL is invalid")
            }
        };
        let prefix_override = MetricsIntakeUrlPrefixOverride::maybe_new(Some(dd_url), None);
        let metrics_intake_url = MetricsIntakeUrlPrefix::new(None, prefix_override)
            .expect("can't parse additional endpoint URL");

        // Create a flusher for each endpoint URL and API key pair
        for api_key in api_keys {
            let additional_api_key_factory = Arc::new(ApiKeyFactory::new(api_key));
            let additional_flusher_config = MetricsFlusherConfig {
                api_key_factory: additional_api_key_factory,
                aggregator_handle: metrics_aggr_handle.clone(),
                metrics_intake_url_prefix: metrics_intake_url.clone(),
                https_proxy: config.proxy_https.clone(),
                ca_cert_path: config.tls_cert_file.clone(),
                timeout: Duration::from_secs(config.flush_timeout),
                retry_strategy: DsdRetryStrategy::Immediate(3),
                compression_level: config.metrics_config_compression_level,
            };
            flushers.push(MetricsFlusher::new(additional_flusher_config));
        }
    }
    flushers
}

async fn setup_telemetry_client(
    client: &Client,
    extension_id: &str,
    runtime_api: &str,
    logs_tx: Sender<TelemetryEvent>,
    event_bus_tx: Sender<Event>,
    logs_enabled: bool,
    managed_instance_mode: bool,
) -> anyhow::Result<CancellationToken> {
    let listener = TelemetryListener::new(EXTENSION_HOST_IP, TELEMETRY_PORT, logs_tx, event_bus_tx);

    let cancel_token = listener.cancel_token();
    match listener.start() {
        Ok(()) => {
            // Drop the listener, so event_bus_tx is closed
            drop(listener);
        }
        Err(e) => {
            error!("Error starting telemetry listener: {e:?}");
        }
    }

    telemetry::subscribe(
        client,
        runtime_api,
        extension_id,
        TELEMETRY_PORT,
        logs_enabled,
        managed_instance_mode,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to subscribe to telemetry: {e:?}"))?;

    Ok(cancel_token)
}

fn start_otlp_agent(
    config: &Arc<Config>,
    tags_provider: Arc<TagProvider>,
    trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendDataBuilderInfo>,
    stats_concentrator: StatsConcentratorHandle,
) -> Option<CancellationToken> {
    if !should_enable_otlp_agent(config) {
        return None;
    }
    let stats_generator = Arc::new(StatsGenerator::new(stats_concentrator));
    let agent = OtlpAgent::new(
        config.clone(),
        tags_provider,
        trace_processor,
        trace_tx,
        stats_generator,
    );
    let cancel_token = agent.cancel_token();

    tokio::spawn(async move {
        if let Err(e) = agent.start().await {
            error!("Error starting OTLP agent: {e:?}");
        }
    });

    Some(cancel_token)
}

fn start_api_runtime_proxy(
    config: &Arc<Config>,
    aws_config: Arc<AwsConfig>,
    invocation_processor_handle: &InvocationProcessorHandle,
    appsec_processor: Option<&Arc<TokioMutex<AppSecProcessor>>>,
    propagator: Arc<DatadogCompositePropagator>,
) -> Option<CancellationToken> {
    if !should_start_proxy(config, Arc::clone(&aws_config)) {
        debug!("Skipping API runtime proxy, no LWA proxy or datadog wrapper found");
        return None;
    }

    let appsec_processor = appsec_processor.map(Arc::clone);
    interceptor::start(
        aws_config,
        invocation_processor_handle.clone(),
        appsec_processor,
        propagator,
    )
    .ok()
}

#[cfg(test)]
mod flush_handles_tests {
    use bottlecap::flushing::FlushHandles;
    use tokio::time::{Duration, sleep};

    #[tokio::test]
    async fn stats_handle_is_tracked_until_completion() {
        let mut handles = FlushHandles::new();
        let handle = tokio::spawn(async {
            sleep(Duration::from_millis(5)).await;
        });
        handles.stats_flush_handles.push(handle);

        assert!(handles.has_pending());

        sleep(Duration::from_millis(10)).await;

        assert!(!handles.has_pending());
    }
}
