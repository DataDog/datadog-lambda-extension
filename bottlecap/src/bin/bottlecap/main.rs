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

use bottlecap::{
    base_url,
    config::{
        self,
        aws::{build_lambda_function_arn, AwsConfig},
        Config,
    },
    event_bus::bus::EventBus,
    events::Event,
    fips::{log_fips_status, prepare_client_provider},
    lifecycle::{
        flush_control::FlushControl, invocation::processor::Processor as InvocationProcessor,
        listener::Listener as LifecycleListener,
    },
    logger,
    logs::{agent::LogsAgent, flusher::Flusher as LogsFlusher},
    otlp::agent::Agent as OtlpAgent,
    proxy::{interceptor, should_start_proxy},
    secrets::decrypt,
    tags::{
        lambda::{self, tags::EXTENSION_VERSION},
        provider::Provider as TagProvider,
    },
    telemetry::{
        client::TelemetryApiClient,
        events::{TelemetryEvent, TelemetryRecord},
        listener::{TelemetryListener, TelemetryListenerConfig},
    },
    traces::{
        stats_aggregator::StatsAggregator,
        stats_flusher::{self, StatsFlusher},
        stats_processor, trace_agent, trace_aggregator,
        trace_flusher::{self, TraceFlusher},
        trace_processor,
    },
    DOGSTATSD_PORT, EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES, EXTENSION_HOST,
    EXTENSION_ID_HEADER, EXTENSION_NAME, EXTENSION_NAME_HEADER, EXTENSION_ROUTE,
    LAMBDA_RUNTIME_SLUG, TELEMETRY_PORT,
};
use datadog_fips::reqwest_adapter::create_reqwest_client_builder;
use datadog_trace_obfuscation::obfuscation_config;
use datadog_trace_utils::send_data::SendData;
use decrypt::resolve_secrets;
use dogstatsd::{
    aggregator::Aggregator as MetricsAggregator,
    constants::CONTEXTS,
    datadog::{
        DdDdUrl, DdUrl, MetricsIntakeUrlPrefix, MetricsIntakeUrlPrefixOverride,
        RetryStrategy as DsdRetryStrategy, Site as MetricsSite,
    },
    dogstatsd::{DogStatsD, DogStatsDConfig},
    flusher::{Flusher as MetricsFlusher, FlusherConfig as MetricsFlusherConfig},
    metric::{SortedTags, EMPTY_TAGS},
};
use reqwest::Client;
use serde::Deserialize;
use std::{
    collections::{hash_map, HashMap},
    env,
    io::{Error, Result},
    os::unix::process::CommandExt,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc::Sender, Mutex as TokioMutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};
use tracing_subscriber::EnvFilter;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterResponse {
    // Skip deserialize because this field is not available in the response
    // body, but as a header. Header is extracted and set manually.
    #[serde(skip_deserializing)]
    extension_id: String,
    account_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "eventType")]
enum NextEventResponse {
    #[serde(rename(deserialize = "INVOKE"))]
    Invoke {
        #[serde(rename(deserialize = "deadlineMs"))]
        deadline_ms: u64,
        #[serde(rename(deserialize = "requestId"))]
        request_id: String,
        #[serde(rename(deserialize = "invokedFunctionArn"))]
        invoked_function_arn: String,
    },
    #[serde(rename(deserialize = "SHUTDOWN"))]
    Shutdown {
        #[serde(rename(deserialize = "shutdownReason"))]
        shutdown_reason: String,
        #[serde(rename(deserialize = "deadlineMs"))]
        deadline_ms: u64,
    },
}

async fn next_event(client: &Client, ext_id: &str) -> Result<NextEventResponse> {
    let base_url = base_url(EXTENSION_ROUTE)
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let url = format!("{base_url}/event/next");

    let response = client
        .get(&url)
        .header(EXTENSION_ID_HEADER, ext_id)
        .send()
        .await
        .map_err(|e| {
            error!("Next request failed: {}", e);
            Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;

    let status = response.status();
    let text = response.text().await.map_err(|e| {
        error!("Next response: Failed to read response body: {}", e);
        Error::new(std::io::ErrorKind::InvalidData, e.to_string())
    })?;

    if !status.is_success() {
        error!("Next response HTTP Error {} - Response: {}", status, text);
        return Err(Error::new(
            std::io::ErrorKind::InvalidData,
            format!("HTTP Error {status}"),
        ));
    }

    serde_json::from_str(&text).map_err(|e| {
        error!("Next JSON parse error on response: {}", text);
        Error::new(std::io::ErrorKind::InvalidData, e.to_string())
    })
}

async fn register(client: &Client) -> Result<RegisterResponse> {
    let mut map = HashMap::new();
    let base_url = base_url(EXTENSION_ROUTE)
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    map.insert("events", vec!["INVOKE", "SHUTDOWN"]);
    let url = format!("{base_url}/register");

    let resp = client
        .post(&url)
        .header(EXTENSION_NAME_HEADER, EXTENSION_NAME)
        .header(EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES)
        .json(&map)
        .send()
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    if resp.status() != 200 {
        let err = resp.error_for_status_ref();
        panic!("Can't register extension {err:?}");
    }

    let extension_id = resp
        .headers()
        .get(EXTENSION_ID_HEADER)
        .expect("Extension ID header not found")
        .to_str()
        .expect("Can't convert header to string")
        .to_string();
    let mut register_response: RegisterResponse = resp
        .json::<RegisterResponse>()
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    // Set manually since it's not part of the response body
    register_response.extension_id = extension_id;

    Ok(register_response)
}

#[tokio::main]
async fn main() -> Result<()> {
    let start_time = Instant::now();
    let (mut aws_config, config) = load_configs(start_time);

    enable_logging_subsystem(&config);
    log_fips_status(&aws_config.region);
    let version_without_next = EXTENSION_VERSION.split('-').next().unwrap_or("NA");
    debug!("Starting Datadog Extension {version_without_next}");
    prepare_client_provider()?;
    let client = create_reqwest_client_builder()
        .map_err(|e| {
            Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to create client builder: {e:?}"),
            )
        })?
        .no_proxy()
        .build()
        .map_err(|e| {
            Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to create client: {e:?}"),
            )
        })?;

    let r = register(&client)
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    if let Some(resolved_api_key) = resolve_secrets(Arc::clone(&config), &mut aws_config).await {
        match extension_loop_active(
            &aws_config,
            &config,
            &client,
            &r,
            resolved_api_key,
            start_time,
        )
        .await
        {
            Ok(()) => {
                debug!("Extension loop completed successfully");
                Ok(())
            }
            Err(e) => {
                error!(
                    "Extension loop failed: {e:?}, Calling /next without Datadog instrumentation"
                );
                extension_loop_idle(&client, &r).await
            }
        }
    } else {
        error!("Failed to resolve secrets, Datadog extension will be idle");
        extension_loop_idle(&client, &r).await
    }
}

fn load_configs(start_time: Instant) -> (AwsConfig, Arc<Config>) {
    // First load the AWS configuration
    let aws_config = AwsConfig::from_env(start_time);
    let lambda_directory: String =
        env::var("LAMBDA_TASK_ROOT").unwrap_or_else(|_| "/var/task".to_string());
    let config = match config::get_config(Path::new(&lambda_directory)) {
        Ok(config) => Arc::new(config),
        Err(_e) => {
            let err = Command::new("/opt/datadog-agent-go").exec();
            panic!("Error starting the extension: {err:?}");
        }
    };

    (aws_config, config)
}

fn enable_logging_subsystem(config: &Arc<Config>) {
    let env_filter = format!(
        "h2=off,hyper=off,reqwest=off,rustls=off,datadog-trace-mini-agent=off,{:?}",
        config.log_level
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

async fn extension_loop_idle(client: &Client, r: &RegisterResponse) -> Result<()> {
    loop {
        match next_event(client, &r.extension_id).await {
            Ok(_) => {
                debug!("Extension is idle, skipping next event");
            }
            Err(e) => {
                error!("Error getting next event: {e:?}");
                return Err(e);
            }
        };
    }
}

#[allow(clippy::too_many_lines)]
async fn extension_loop_active(
    aws_config: &AwsConfig,
    config: &Arc<Config>,
    client: &Client,
    r: &RegisterResponse,
    resolved_api_key: String,
    start_time: Instant,
) -> Result<()> {
    let mut event_bus = EventBus::run();

    let account_id = r
        .account_id
        .as_ref()
        .unwrap_or(&"none".to_string())
        .to_string();
    let tags_provider = setup_tag_provider(aws_config, config, &account_id);

    let (logs_agent_channel, logs_flusher) = start_logs_agent(
        config,
        resolved_api_key.clone(),
        &tags_provider,
        event_bus.get_sender_copy(),
    );

    let metrics_aggr = Arc::new(Mutex::new(
        MetricsAggregator::new(
            SortedTags::parse(&tags_provider.get_tags_string()).unwrap_or(EMPTY_TAGS),
            CONTEXTS,
        )
        .expect("failed to create aggregator"),
    ));

    let mut metrics_flushers = start_metrics_flusher(&resolved_api_key, &metrics_aggr, config);
    // Lifecycle Invocation Processor
    let invocation_processor = Arc::new(TokioMutex::new(InvocationProcessor::new(
        Arc::clone(&tags_provider),
        Arc::clone(config),
        aws_config,
        Arc::clone(&metrics_aggr),
    )));

    let trace_aggregator = Arc::new(TokioMutex::new(trace_aggregator::TraceAggregator::default()));
    let (trace_agent_channel, trace_flusher, trace_processor, stats_flusher) = start_trace_agent(
        config,
        resolved_api_key.clone(),
        &tags_provider,
        Arc::clone(&invocation_processor),
        Arc::clone(&trace_aggregator),
    );

    let api_runtime_proxy_shutdown_signal =
        start_api_runtime_proxy(config, aws_config, &invocation_processor);

    let lifecycle_listener = LifecycleListener {
        invocation_processor: Arc::clone(&invocation_processor),
    };
    // TODO(astuyve): deprioritize this task after the first request
    tokio::spawn(async move {
        let res = lifecycle_listener.start().await;
        if let Err(e) = res {
            error!("Error starting hello agent: {e:?}");
        }
    });

    let dogstatsd_cancel_token = start_dogstatsd(&metrics_aggr).await;

    let telemetry_listener_cancel_token =
        setup_telemetry_client(&r.extension_id, logs_agent_channel).await?;

    start_otlp_agent(
        config,
        tags_provider.clone(),
        trace_processor.clone(),
        trace_agent_channel.clone(),
    );

    let mut flush_control = FlushControl::new(config.serverless_flush_strategy);

    let mut race_flush_interval = flush_control.get_flush_interval();
    race_flush_interval.tick().await; // discard first tick, which is instantaneous

    debug!(
        "Datadog Next-Gen Extension ready in {:}ms",
        start_time.elapsed().as_millis().to_string()
    );
    // first invoke we must call next
    let next_lambda_response = next_event(client, &r.extension_id).await;

    handle_next_invocation(next_lambda_response, invocation_processor.clone()).await;
    loop {
        let shutdown;

        if flush_control.should_flush_end() {
            // break loop after runtime done
            // flush everything
            // call next
            // optionally flush after tick for long running invos
            'flush_end: loop {
                tokio::select! {
                biased;
                    Some(event) = event_bus.rx.recv() => {
                        if let Some(telemetry_event) = handle_event_bus_event(event, invocation_processor.clone(), tags_provider.clone(), trace_processor.clone(), trace_agent_channel.clone()).await {
                            if let TelemetryRecord::PlatformRuntimeDone{ .. } = telemetry_event.record {
                                break 'flush_end;
                            }
                        }
                    }
                    _ = race_flush_interval.tick() => {
                        flush_all(
                            &logs_flusher,
                            &mut metrics_flushers,
                            &*trace_flusher,
                            &*stats_flusher,
                            &mut race_flush_interval,
                        ).await;
                    }
                }
            }
            // flush
            flush_all(
                &logs_flusher,
                &mut metrics_flushers,
                &*trace_flusher,
                &*stats_flusher,
                &mut race_flush_interval,
            )
            .await;
            let next_response = next_event(client, &r.extension_id).await;
            shutdown = handle_next_invocation(next_response, invocation_processor.clone()).await;
        } else {
            //Periodic flush scenario, flush at top of invocation
            if flush_control.should_periodic_flush() {
                // Should flush at the top of the invocation, which is now
                flush_all(
                    &logs_flusher,
                    &mut metrics_flushers,
                    &*trace_flusher,
                    &*stats_flusher,
                    &mut race_flush_interval,
                )
                .await;
            }
            // NO FLUSH SCENARIO
            // JUST LOOP OVER PIPELINE AND WAIT FOR NEXT EVENT
            // If we get platform.runtimeDone or platform.runtimeReport
            // That's fine, we still wait to break until we get the response from next
            // and then we break to determine if we'll flush or not
            let next_lambda_response = next_event(client, &r.extension_id);
            tokio::pin!(next_lambda_response);
            'next_invocation: loop {
                tokio::select! {
                biased;
                    next_response = &mut next_lambda_response => {
                        // Dear reader this is important, you may be tempted to remove this
                        // after all, why reset the flush interval if we're not flushing?
                        // It's because the race_flush_interval is only for the RACE FLUSH
                        // For long-running txns. The call to `flush_control.should_flush_end()`
                        // has its own interval which is not reset here.
                        race_flush_interval.reset();
                        // Thank you for not removing race_flush_interval.reset();

                        shutdown = handle_next_invocation(next_response, invocation_processor.clone()).await;
                        // Need to break here to re-call next
                        break 'next_invocation;
                    }
                    Some(event) = event_bus.rx.recv() => {
                        handle_event_bus_event(event, invocation_processor.clone(), tags_provider.clone(), trace_processor.clone(), trace_agent_channel.clone()).await;
                    }
                    _ = race_flush_interval.tick() => {
                        flush_all(
                            &logs_flusher,
                            &mut metrics_flushers,
                            &*trace_flusher,
                            &*stats_flusher,
                            &mut race_flush_interval,
                        ).await;
                    }
                }
            }
        }

        if shutdown {
            'shutdown: loop {
                tokio::select! {
                    Some(event) = event_bus.rx.recv() => {
                            if let Some(telemetry_event) = handle_event_bus_event(event, invocation_processor.clone(), tags_provider.clone(), trace_processor.clone(), trace_agent_channel.clone()).await {
                            if let TelemetryRecord::PlatformReport{ .. } = telemetry_event.record {
                                // Wait for the report event before shutting down
                                break 'shutdown;
                            }
                        }
                    }
                }
            }

            if let Some(api_runtime_proxy_cancel_token) = api_runtime_proxy_shutdown_signal {
                api_runtime_proxy_cancel_token.cancel();
            }
            dogstatsd_cancel_token.cancel();
            telemetry_listener_cancel_token.cancel();
            flush_all(
                &logs_flusher,
                &mut metrics_flushers,
                &*trace_flusher,
                &*stats_flusher,
                &mut race_flush_interval,
            )
            .await;
            return Ok(());
        }
    }
}

async fn flush_all(
    logs_flusher: &LogsFlusher,
    metrics_flushers: &mut [MetricsFlusher],
    trace_flusher: &impl TraceFlusher,
    stats_flusher: &impl StatsFlusher,
    race_flush_interval: &mut tokio::time::Interval,
) {
    println!("=== Starting flush_all for {} metrics flushers ===", metrics_flushers.len());
    let metrics_futures: Vec<_> = metrics_flushers
        .iter_mut()
        .map(MetricsFlusher::flush)
        .collect();
    tokio::join!(
        logs_flusher.flush(),
        futures::future::join_all(metrics_futures),
        trace_flusher.flush(),
        stats_flusher.flush()
    );
    println!("=== Completed flush_all ===");
    race_flush_interval.reset();
}

async fn handle_event_bus_event(
    event: Event,
    invocation_processor: Arc<TokioMutex<InvocationProcessor>>,
    tags_provider: Arc<TagProvider>,
    trace_processor: Arc<trace_processor::ServerlessTraceProcessor>,
    trace_agent_channel: Sender<datadog_trace_utils::send_data::SendData>,
) -> Option<TelemetryEvent> {
    match event {
        Event::Metric(event) => {
            debug!("Metric event: {:?}", event);
        }
        Event::OutOfMemory(event_timestamp) => {
            let mut p = invocation_processor.lock().await;
            p.on_out_of_memory_error(event_timestamp);
            drop(p);
        }
        Event::Telemetry(event) => {
            debug!("Telemetry event received: {:?}", event);
            match event.record {
                TelemetryRecord::PlatformInitStart { .. } => {
                    let mut p = invocation_processor.lock().await;
                    p.on_platform_init_start(event.time);
                    drop(p);
                }
                TelemetryRecord::PlatformInitReport { metrics, .. } => {
                    let mut p = invocation_processor.lock().await;
                    p.on_platform_init_report(metrics.duration_ms, event.time.timestamp());
                    drop(p);
                }
                TelemetryRecord::PlatformStart { request_id, .. } => {
                    let mut p = invocation_processor.lock().await;
                    p.on_platform_start(request_id, event.time);
                    drop(p);
                }
                TelemetryRecord::PlatformRuntimeDone {
                    ref request_id,
                    metrics: Some(metrics),
                    status,
                    ..
                } => {
                    let mut p = invocation_processor.lock().await;
                    p.on_platform_runtime_done(
                        request_id,
                        metrics,
                        status,
                        tags_provider.clone(),
                        trace_processor.clone(),
                        trace_agent_channel.clone(),
                        event.time.timestamp(),
                    )
                    .await;
                    drop(p);
                    return Some(event);
                }
                TelemetryRecord::PlatformReport {
                    ref request_id,
                    metrics,
                    ..
                } => {
                    let mut p = invocation_processor.lock().await;
                    p.on_platform_report(request_id, metrics, event.time.timestamp());
                    drop(p);
                    return Some(event);
                }
                _ => {
                    debug!("Unforwarded Telemetry event: {:?}", event);
                }
            }
        }
    }
    None
}

async fn handle_next_invocation(
    next_response: Result<NextEventResponse>,
    invocation_processor: Arc<TokioMutex<InvocationProcessor>>,
) -> bool {
    match next_response {
        Ok(NextEventResponse::Invoke {
            request_id,
            deadline_ms,
            invoked_function_arn,
        }) => {
            debug!(
                "Invoke event {}; deadline: {}, invoked_function_arn: {}",
                request_id, deadline_ms, invoked_function_arn
            );
            let mut p = invocation_processor.lock().await;
            p.on_invoke_event(request_id);
            drop(p);
            false
        }
        Ok(NextEventResponse::Shutdown {
            shutdown_reason,
            deadline_ms,
        }) => {
            println!("Exiting: {shutdown_reason}, deadline: {deadline_ms}");
            true
        }
        Err(err) => {
            eprintln!("Error: {err:?}");
            println!("Exiting");
            true
        }
    }
}

fn setup_tag_provider(
    aws_config: &AwsConfig,
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
    resolved_api_key: String,
    tags_provider: &Arc<TagProvider>,
    event_bus: Sender<Event>,
) -> (Sender<TelemetryEvent>, LogsFlusher) {
    let mut logs_agent = LogsAgent::new(Arc::clone(tags_provider), Arc::clone(config), event_bus);
    let logs_agent_channel = logs_agent.get_sender_copy();
    let logs_flusher = LogsFlusher::new(
        resolved_api_key,
        Arc::clone(&logs_agent.aggregator),
        config.clone(),
    );
    tokio::spawn(async move {
        logs_agent.spin().await;
    });
    (logs_agent_channel, logs_flusher)
}

fn start_metrics_flusher(
    resolved_api_key: &str,
    metrics_aggr: &Arc<Mutex<MetricsAggregator>>,
    config: &Arc<Config>,
) -> Vec<MetricsFlusher> {
    let mut flushers = Vec::new();

    // Create primary flusher
    let primary_metrics_intake_url = if !config.dd_url.is_empty() {
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

    let primary_flusher_config = MetricsFlusherConfig {
        api_key: resolved_api_key.to_string(),
        aggregator: metrics_aggr.clone(),
        metrics_intake_url_prefix: primary_metrics_intake_url
            .expect("can't parse site or override"),
        https_proxy: config.https_proxy.clone(),
        timeout: Duration::from_secs(config.flush_timeout),
        retry_strategy: DsdRetryStrategy::Immediate(3),
    };
    flushers.push(MetricsFlusher::new(primary_flusher_config));

    // Create additional flushers for dual shipping
    for (endpoint_url, api_keys) in &config.additional_endpoints {
        println!("=== Additional endpoint URL: {} ===", endpoint_url);
        let dd_url = DdUrl::new(endpoint_url.clone()).expect("can't parse additional endpoint URL");
        let prefix_override = MetricsIntakeUrlPrefixOverride::maybe_new(Some(dd_url), None);
        let metrics_intake_url = MetricsIntakeUrlPrefix::new(None, prefix_override)
            .expect("can't parse additional endpoint URL");

        // Create a flusher for each API key
        for api_key in api_keys {
            let additional_flusher_config = MetricsFlusherConfig {
                api_key: api_key.clone(),
                aggregator: metrics_aggr.clone(),
                metrics_intake_url_prefix: metrics_intake_url.clone(),
                https_proxy: config.https_proxy.clone(),
                timeout: Duration::from_secs(config.flush_timeout),
                retry_strategy: DsdRetryStrategy::Immediate(3),
            };
            flushers.push(MetricsFlusher::new(additional_flusher_config));
        }
    }

    println!("=== Number of Metrics flushers: {:?} ===", flushers.len());

    flushers
}

fn start_trace_agent(
    config: &Arc<Config>,
    resolved_api_key: String,
    tags_provider: &Arc<TagProvider>,
    invocation_processor: Arc<TokioMutex<InvocationProcessor>>,
    trace_aggregator: Arc<TokioMutex<trace_aggregator::TraceAggregator>>,
) -> (
    Sender<datadog_trace_utils::send_data::SendData>,
    Arc<trace_flusher::ServerlessTraceFlusher>,
    Arc<trace_processor::ServerlessTraceProcessor>,
    Arc<stats_flusher::ServerlessStatsFlusher>,
) {
    // Stats
    let stats_aggregator = Arc::new(TokioMutex::new(StatsAggregator::default()));
    let stats_flusher = Arc::new(stats_flusher::ServerlessStatsFlusher::new(
        resolved_api_key.clone(),
        stats_aggregator.clone(),
        Arc::clone(config),
    ));

    let stats_processor = Arc::new(stats_processor::ServerlessStatsProcessor {});

    // Traces
    let trace_flusher = Arc::new(trace_flusher::ServerlessTraceFlusher {
        aggregator: trace_aggregator.clone(),
        config: Arc::clone(config),
    });

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
        resolved_api_key: resolved_api_key.clone(),
    });

    let trace_agent = Box::new(trace_agent::TraceAgent::new(
        Arc::clone(config),
        trace_aggregator,
        trace_processor.clone(),
        stats_aggregator,
        stats_processor,
        invocation_processor,
        Arc::clone(tags_provider),
        resolved_api_key,
    ));
    let trace_agent_channel = trace_agent.get_sender_copy();

    tokio::spawn(async move {
        let res = trace_agent.start().await;
        if let Err(e) = res {
            error!("Error starting trace agent: {e:?}");
        }
    });

    (
        trace_agent_channel,
        trace_flusher,
        trace_processor,
        stats_flusher,
    )
}

async fn start_dogstatsd(metrics_aggr: &Arc<Mutex<MetricsAggregator>>) -> CancellationToken {
    let dogstatsd_config = DogStatsDConfig {
        host: EXTENSION_HOST.to_string(),
        port: DOGSTATSD_PORT,
    };
    let dogstatsd_cancel_token = tokio_util::sync::CancellationToken::new();
    let dogstatsd_client = DogStatsD::new(
        &dogstatsd_config,
        Arc::clone(metrics_aggr),
        dogstatsd_cancel_token.clone(),
    )
    .await;

    tokio::spawn(async move {
        dogstatsd_client.spin().await;
    });

    dogstatsd_cancel_token
}

async fn setup_telemetry_client(
    extension_id: &str,
    logs_agent_channel: Sender<TelemetryEvent>,
) -> Result<CancellationToken> {
    let telemetry_listener_config = TelemetryListenerConfig {
        host: EXTENSION_HOST.to_string(),
        port: TELEMETRY_PORT,
    };
    let telemetry_listener_cancel_token = tokio_util::sync::CancellationToken::new();
    let ct_clone = telemetry_listener_cancel_token.clone();
    tokio::spawn(async move {
        let _ =
            TelemetryListener::spin(&telemetry_listener_config, logs_agent_channel, ct_clone).await;
    });

    let telemetry_client = TelemetryApiClient::new(extension_id.to_string(), TELEMETRY_PORT);
    telemetry_client
        .subscribe()
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(telemetry_listener_cancel_token)
}

fn start_otlp_agent(
    config: &Arc<Config>,
    tags_provider: Arc<TagProvider>,
    trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
    trace_tx: Sender<SendData>,
) {
    if !config.otlp_config_traces_enabled {
        return;
    }

    let agent = OtlpAgent::new(config.clone(), tags_provider, trace_processor, trace_tx);

    tokio::spawn(async move {
        if let Err(e) = agent.start().await {
            error!("Error starting OTLP agent: {e:?}");
        }
    });
}

fn start_api_runtime_proxy(
    config: &Arc<Config>,
    aws_config: &AwsConfig,
    invocation_processor: &Arc<TokioMutex<InvocationProcessor>>,
) -> Option<CancellationToken> {
    if !should_start_proxy(config, aws_config) {
        debug!("Skipping API runtime proxy, no LWA proxy or datadog wrapper found");
        return None;
    }

    let aws_config = aws_config.clone();
    let invocation_processor = invocation_processor.clone();
    interceptor::start(aws_config, invocation_processor).ok()
}
