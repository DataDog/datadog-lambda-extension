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
    config::{self, get_aws_partition_by_region, AwsConfig, Config},
    event_bus::bus::EventBus,
    events::Event,
    lifecycle::{
        flush_control::FlushControl, invocation::processor::Processor as InvocationProcessor,
        listener::Listener as LifecycleListener,
    },
    logger,
    logs::{
        agent::LogsAgent,
        flusher::{build_fqdn_logs, Flusher as LogsFlusher},
    },
    secrets::decrypt,
    tags::{lambda, provider::Provider as TagProvider},
    telemetry::{
        self,
        client::TelemetryApiClient,
        events::{RuntimeDoneMetrics, Status, TelemetryEvent, TelemetryRecord},
        listener::TelemetryListener,
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
use datadog_trace_obfuscation::obfuscation_config;
use decrypt::resolve_secrets;
use dogstatsd::{
    aggregator::Aggregator as MetricsAggregator,
    constants::CONTEXTS,
    datadog::{MetricsIntakeUrlPrefix, Site as MetricsSite},
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
    time::Duration,
    time::Instant,
};
use telemetry::listener::TelemetryListenerConfig;
use tokio::{sync::mpsc::Sender, sync::Mutex as TokioMutex};
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

async fn next_event(client: &reqwest::Client, ext_id: &str) -> Result<NextEventResponse> {
    let base_url = base_url(EXTENSION_ROUTE)
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let url = format!("{base_url}/event/next");

    let response = client
        .get(&url)
        .header(EXTENSION_ID_HEADER, ext_id)
        .send()
        .await
        .map_err(|e| {
            debug!("Request failed: {}", e);
            Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;

    let status = response.status();
    let text = response.text().await.map_err(|e| {
        debug!("Failed to read response body: {}", e);
        Error::new(std::io::ErrorKind::InvalidData, e.to_string())
    })?;

    if !status.is_success() {
        debug!("HTTP Error {} - Response: {}", status, text);
        return Err(Error::new(
            std::io::ErrorKind::InvalidData,
            format!("HTTP Error {}", status),
        ));
    }

    serde_json::from_str(&text).map_err(|e| {
        debug!("JSON parse error on response: {}", text);
        Error::new(std::io::ErrorKind::InvalidData, e.to_string())
    })
}

async fn register(client: &reqwest::Client) -> Result<RegisterResponse> {
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

fn build_function_arn(account_id: &str, region: &str, function_name: &str) -> String {
    let aws_partition = get_aws_partition_by_region(region);
    format!("arn:{aws_partition}:lambda:{region}:{account_id}:function:{function_name}")
}

#[tokio::main]
async fn main() -> Result<()> {
    let (mut aws_config, config) = load_configs();

    enable_logging_subsystem(&config);
    let client = reqwest::Client::builder().no_proxy().build().map_err(|e| {
        Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Failed to create client: {e:?}"),
        )
    })?;

    let r = register(&client)
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    if let Some(resolved_api_key) = resolve_secrets(Arc::clone(&config), &mut aws_config).await {
        match extension_loop_active(&aws_config, &config, &client, &r, resolved_api_key).await {
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

fn load_configs() -> (AwsConfig, Arc<Config>) {
    // First load the configuration
    let aws_config = AwsConfig {
        region: env::var("AWS_DEFAULT_REGION").unwrap_or("us-east-1".to_string()),
        aws_access_key_id: env::var("AWS_ACCESS_KEY_ID").unwrap_or_default(),
        aws_secret_access_key: env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default(),
        aws_session_token: env::var("AWS_SESSION_TOKEN").unwrap_or_default(),
        aws_container_credentials_full_uri: env::var("AWS_CONTAINER_CREDENTIALS_FULL_URI")
            .unwrap_or_default(),
        aws_container_authorization_token: env::var("AWS_CONTAINER_AUTHORIZATION_TOKEN")
            .unwrap_or_default(),
        function_name: env::var("AWS_LAMBDA_FUNCTION_NAME").unwrap_or_default(),
        sandbox_init_time: Instant::now(),
    };
    let lambda_directory = env::var("LAMBDA_TASK_ROOT").unwrap_or_else(|_| "/var/task".to_string());
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
        "h2=off,hyper=off,rustls=off,datadog-trace-mini-agent=off,{:?}",
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

    let metrics_site = MetricsSite::new(config.site.clone()).expect("can't parse site");
    let flusher_config = MetricsFlusherConfig {
        api_key: resolved_api_key.clone(),
        aggregator: Arc::clone(&metrics_aggr),
        metrics_intake_url_prefix: MetricsIntakeUrlPrefix::new(Some(metrics_site), None)
            .expect("can't parse metrics intake URL from site"),
        https_proxy: config.https_proxy.clone(),
        timeout: Duration::from_secs(config.flush_timeout),
    };
    let mut metrics_flusher = MetricsFlusher::new(flusher_config);

    // Lifecycle Invocation Processor
    let invocation_processor = Arc::new(TokioMutex::new(InvocationProcessor::new(
        Arc::clone(&tags_provider),
        Arc::clone(config),
        aws_config,
        Arc::clone(&metrics_aggr),
    )));

    let (trace_agent_channel, trace_flusher, trace_processor, stats_flusher) =
        start_trace_agent(config, resolved_api_key.clone(), &tags_provider);

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

    let mut flush_control = FlushControl::new(config.serverless_flush_strategy);

    let mut flush_interval = flush_control.get_flush_interval();
    flush_interval.tick().await; // discard first tick, which is instantaneous

    // first invoke we must call next
    let next_lambda_response = next_event(client, &r.extension_id).await;

    handle_next_invocation(next_lambda_response, invocation_processor.clone()).await;
    loop {
        let shutdown;
        // Don't await! Pin it and check in the tokio loop
        // Tokio select! drops whatever task does not complete first
        // but the state machine API in Lambda will give us an invalid error
        // if we call /next twice instead of waiting
        //let mut next_response_received = false;
        if flush_control.should_flush_end() {
            // break loop after runtime done
            // flush everything
            // call next
            // optionally flush after tick for long running invos
            'inner: loop {
                tokio::select! {
                biased;
                    Some(event) = event_bus.rx.recv() => {
                        if let Some(runtime_done_meta) = handle_event_bus_event(event, invocation_processor.clone()).await {
                            let mut p = invocation_processor.lock().await;
                            p.on_platform_runtime_done(
                                &runtime_done_meta.request_id,
                                runtime_done_meta.metrics.duration_ms,
                                runtime_done_meta.status,
                                config.clone(),
                                tags_provider.clone(),
                                trace_processor.clone(),
                                trace_agent_channel.clone()
                            ).await;
                            drop(p);
                            break 'inner;
                        }
                    }
                    _ = flush_interval.tick() => {
                        let race_flush_time = Instant::now();
                        tokio::join!(
                            logs_flusher.flush(),
                            metrics_flusher.flush(),
                            trace_flusher.flush(),
                            stats_flusher.flush()
                        );
                        println!(
                            "RACE FLUSH at end data in {}ms",
                            race_flush_time.elapsed().as_millis()
                        );
                    }
                }
            }
            let flush_start_time = Instant::now();
            // flush
            tokio::join!(
                logs_flusher.flush(),
                metrics_flusher.flush(),
                trace_flusher.flush(),
                stats_flusher.flush()
            );
            println!(
                "Flushed all data in {}ms",
                flush_start_time.elapsed().as_millis()
            );
 
            flush_interval.reset();
            let next_lambda_response = next_event(client, &r.extension_id).await;

            shutdown =
                handle_next_invocation(next_lambda_response, invocation_processor.clone()).await;
        } else {
            // NO FLUSH SCENARIO
            // JUST LOOP OVER PIPELINE AND WAIT FOR NEXT EVENT
            // If we get platform.runtimeDone or platform.runtimeReport
            // That's fine, we still wait to break until we get the response from next
            // and then we break to determine if we'll flush or not
            let next_lambda_response = next_event(client, &r.extension_id);
            tokio::pin!(next_lambda_response);
            'inner: loop {
                tokio::select! {
                biased;
                    next_response = &mut next_lambda_response => {
                        // Dear reader this is important, you may be tempted to remove this
                        // after all, why reset the flush interval if we're not flushing?
                        // It's because the flush_interval is only for the RACE FLUSH
                        // For long-running txns. The call to `flush_control.should_flush_end()`
                        // has its own interval which is not reset here.
                        flush_interval.reset();
                        // Thank you for not removing flush_interval.reset();

                        shutdown = handle_next_invocation(next_response, invocation_processor.clone()).await;
                        // Need to break here to re-call next
                        break 'inner;
                    }
                    Some(event) = event_bus.rx.recv() => {
                        if let Some(runtime_done_meta) = handle_event_bus_event(event, invocation_processor.clone()).await {
                            let mut p = invocation_processor.lock().await;
                            p.on_platform_runtime_done(
                                &runtime_done_meta.request_id,
                                runtime_done_meta.metrics.duration_ms,
                                runtime_done_meta.status,
                                config.clone(),
                                tags_provider.clone(),
                                trace_processor.clone(),
                                trace_agent_channel.clone()
                            ).await;
                            drop(p);
                        }
                    }
                    _ = flush_interval.tick() => {
                        let race_flush_no_flush_start_time = Instant::now();
                        tokio::join!(
                            logs_flusher.flush(),
                            metrics_flusher.flush(),
                            trace_flusher.flush(),
                            stats_flusher.flush()
                        );
                        flush_interval.reset();
                        println!(
                            "RACE NO FLUSH in {}ms",
                            race_flush_no_flush_start_time.elapsed().as_millis()
                        );
                    }
                }
            }
        }

        if shutdown {
            dogstatsd_cancel_token.cancel();
            telemetry_listener_cancel_token.cancel();
            tokio::join!(
                logs_flusher.flush(),
                metrics_flusher.flush(),
                trace_flusher.flush(),
                stats_flusher.flush()
            );
            return Ok(());
        }
    }
}

struct RuntimeDoneMeta {
    request_id: String,
    status: Status,
    metrics: RuntimeDoneMetrics,
}

async fn handle_event_bus_event(
    event: Event,
    invocation_processor: Arc<TokioMutex<InvocationProcessor>>,
) -> Option<RuntimeDoneMeta> {
    match event {
        Event::Metric(event) => {
            debug!("Metric event: {:?}", event);
            None
        }
        Event::OutOfMemory => {
            let mut p = invocation_processor.lock().await;
            p.on_out_of_memory_error();
            drop(p);
            None
        }
        Event::Telemetry(event) => match event.record {
            TelemetryRecord::PlatformInitStart { .. } => {
                let mut p = invocation_processor.lock().await;
                p.on_platform_init_start(event.time);
                drop(p);
                None
            }
            TelemetryRecord::PlatformInitReport {
                initialization_type,
                phase,
                metrics,
            } => {
                debug!("Platform init report for initialization_type: {:?} with phase: {:?} and metrics: {:?}", initialization_type, phase, metrics);
                let mut p = invocation_processor.lock().await;
                p.on_platform_init_report(metrics.duration_ms);
                drop(p);
                None
            }
            TelemetryRecord::PlatformStart { request_id, .. } => {
                let mut p = invocation_processor.lock().await;
                p.on_platform_start(request_id, event.time);
                drop(p);
                None
            }
            TelemetryRecord::PlatformRuntimeDone {
                request_id,
                status,
                metrics,
                error_type,
                ..
            } => {
                debug!(
                    "Runtime done for request_id: {:?} with status: {:?} and error: {:?}",
                    request_id,
                    status,
                    error_type.unwrap_or("None".to_string())
                );

                if let Some(metrics) = metrics {
                    return Some(RuntimeDoneMeta {
                        request_id,
                        status,
                        metrics,
                    });
                }
                None
            }
            TelemetryRecord::PlatformReport {
                request_id,
                status,
                metrics,
                error_type,
                ..
            } => {
                debug!(
                    "Platform report for request_id: {:?} with status: {:?} and error: {:?}",
                    request_id,
                    status,
                    error_type.unwrap_or("None".to_string())
                );
                let mut p = invocation_processor.lock().await;
                p.on_platform_report(&request_id, metrics);
                drop(p);
                None
            }
            _ => {
                debug!("Unforwarded Telemetry event: {:?}", event);
                None
            }
        },
    }
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
        build_function_arn(account_id, &aws_config.region, &aws_config.function_name);
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
        build_fqdn_logs(config.site.clone()),
        config.clone(),
    );
    tokio::spawn(async move {
        logs_agent.spin().await;
    });
    (logs_agent_channel, logs_flusher)
}

fn start_trace_agent(
    config: &Arc<Config>,
    resolved_api_key: String,
    tags_provider: &Arc<TagProvider>,
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
    let trace_aggregator = Arc::new(TokioMutex::new(trace_aggregator::TraceAggregator::default()));
    let trace_flusher = Arc::new(trace_flusher::ServerlessTraceFlusher {
        aggregator: trace_aggregator.clone(),
        config: Arc::clone(config),
    });

    let obfuscation_config = obfuscation_config::ObfuscationConfig::new()
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
        .expect("Failed to create obfuscation config for Trace Agent");

    let trace_processor = Arc::new(trace_processor::ServerlessTraceProcessor {
        obfuscation_config: Arc::new(obfuscation_config),
        resolved_api_key,
    });

    let trace_agent = Box::new(trace_agent::TraceAgent::new(
        Arc::clone(config),
        trace_aggregator,
        trace_processor.clone(),
        stats_aggregator,
        stats_processor,
        Arc::clone(tags_provider),
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
        TelemetryListener::spin(&telemetry_listener_config, logs_agent_channel, ct_clone).await;
    });

    let telemetry_client = TelemetryApiClient::new(extension_id.to_string(), TELEMETRY_PORT);
    telemetry_client
        .subscribe()
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(telemetry_listener_cancel_token)
}
