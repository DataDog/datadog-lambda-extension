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
    config::{self, AwsConfig, Config},
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
        events::{TelemetryEvent, TelemetryRecord},
        listener::TelemetryListener,
    },
    traces::{
        stats_flusher::{self, StatsFlusher},
        stats_processor, trace_agent,
        trace_flusher::{self, TraceFlusher},
        trace_processor,
    },
    DOGSTATSD_PORT, EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES, EXTENSION_HOST,
    EXTENSION_ID_HEADER, EXTENSION_NAME, EXTENSION_NAME_HEADER, EXTENSION_ROUTE,
    LAMBDA_RUNTIME_SLUG, TELEMETRY_PORT,
};
use datadog_trace_obfuscation::obfuscation_config;
use decrypt::resolve_secrets;
use dogstatsd::metric::{SortedTags, EMPTY_TAGS};
use dogstatsd::{
    aggregator::Aggregator as MetricsAggregator,
    constants::CONTEXTS,
    dogstatsd::{DogStatsD, DogStatsDConfig},
    flusher::{build_fqdn_metrics, Flusher as MetricsFlusher},
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
    time::Instant,
};
use telemetry::listener::TelemetryListenerConfig;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex as TokioMutex;
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
    client
        .get(&url)
        .header(EXTENSION_ID_HEADER, ext_id)
        .send()
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
        .json()
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
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
    format!("arn:aws:lambda:{region}:{account_id}:function:{function_name}")
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (aws_config, config) = load_configs();

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

    if let Some(resolved_api_key) = resolve_secrets(Arc::clone(&config), &aws_config).await {
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
    let mut metrics_flusher = MetricsFlusher::new(
        resolved_api_key.clone(),
        Arc::clone(&metrics_aggr),
        build_fqdn_metrics(config.site.clone()),
        config.https_proxy.clone(),
    );

    let trace_flusher = Arc::new(trace_flusher::ServerlessTraceFlusher {
        buffer: Arc::new(TokioMutex::new(Vec::new())),
    });

    // Lifecycle Invocation Processor
    let invocation_processor = Arc::new(TokioMutex::new(InvocationProcessor::new(
        Arc::clone(&tags_provider),
        Arc::clone(config),
        aws_config,
        Arc::clone(&metrics_aggr),
    )));
    let trace_processor = Arc::new(trace_processor::ServerlessTraceProcessor {
        obfuscation_config: Arc::new(
            obfuscation_config::ObfuscationConfig::new()
                .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?,
        ),
        resolved_api_key: resolved_api_key.clone(),
    });

    let stats_flusher = Arc::new(stats_flusher::ServerlessStatsFlusher {
        buffer: Arc::new(TokioMutex::new(Vec::new())),
        config: Arc::clone(config),
        resolved_api_key: resolved_api_key.clone(),
    });
    let stats_processor = Arc::new(stats_processor::ServerlessStatsProcessor {});

    let trace_flusher_clone = trace_flusher.clone();
    let stats_flusher_clone = stats_flusher.clone();

    let trace_agent = Box::new(
        trace_agent::TraceAgent::new(
            Arc::clone(config),
            trace_processor.clone(),
            trace_flusher_clone,
            stats_processor,
            stats_flusher_clone,
            Arc::clone(&tags_provider),
        )
        .await,
    );
    let trace_agent_tx = trace_agent.get_sender_copy();

    tokio::spawn(async move {
        let res = trace_agent.start().await;
        if let Err(e) = res {
            error!("Error starting trace agent: {e:?}");
        }
    });

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

    let flush_control = FlushControl::new(config.serverless_flush_strategy);
    let mut shutdown = false;

    let mut flush_interval = flush_control.get_flush_interval();
    flush_interval.tick().await; // discard first tick, which is instantaneous

    loop {
        let evt = next_event(client, &r.extension_id).await;
        match evt {
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
            }
            Ok(NextEventResponse::Shutdown {
                shutdown_reason,
                deadline_ms,
            }) => {
                println!("Exiting: {shutdown_reason}, deadline: {deadline_ms}");
                shutdown = true;
            }
            Err(err) => {
                eprintln!("Error: {err:?}");
                println!("Exiting");
                return Err(err);
            }
        }
        // Block until we get something from the telemetry API
        // Check if flush logic says we should block and flush or not
        loop {
            tokio::select! {
            biased;
                Some(event) = event_bus.rx.recv() => {
                    match event {
                        Event::Metric(event) => {
                            debug!("Metric event: {:?}", event);
                        }
                        Event::Telemetry(event) =>
                            match event.record {
                                TelemetryRecord::PlatformInitStart { .. } => {
                                    let mut p = invocation_processor.lock().await;
                                    p.on_platform_init_start(event.time);
                                    drop(p);
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
                                }
                                TelemetryRecord::PlatformStart { request_id, .. } => {
                                    let mut p = invocation_processor.lock().await;
                                    p.on_platform_start(request_id, event.time);
                                    drop(p);
                                }
                                TelemetryRecord::PlatformRuntimeDone {
                                    request_id,
                                    status,
                                    metrics,
                                    ..
                                } => {
                                    debug!(
                                        "Runtime done for request_id: {:?} with status: {:?}",
                                        request_id, status
                                    );

                                    let mut p = invocation_processor.lock().await;
                                    if let Some(metrics) = metrics {
                                        p.on_platform_runtime_done(
                                            &request_id,
                                            metrics.duration_ms,
                                            status,
                                            config.clone(),
                                            tags_provider.clone(),
                                            trace_processor.clone(),
                                            trace_agent_tx.clone()
                                        ).await;
                                    }
                                    drop(p);

                                    // TODO(astuyve) it'll be easy to
                                    // pass the invocation deadline to
                                    // flush tasks here, so they can
                                    // retry if we have more time
                                    if flush_control.should_flush_end() {
                                        tokio::join!(
                                            logs_flusher.flush(),
                                            metrics_flusher.flush(),
                                            trace_flusher.manual_flush(),
                                            stats_flusher.manual_flush()
                                        );
                                    }

                                    break;
                                }
                                TelemetryRecord::PlatformReport {
                                    request_id,
                                    status,
                                    metrics,
                                    ..
                                } => {
                                    debug!(
                                        "Platform report for request_id: {:?} with status: {:?}",
                                        request_id, status
                                    );
                                    let mut p = invocation_processor.lock().await;
                                    p.on_platform_report(&request_id, metrics);
                                    drop(p);

                                    if shutdown {
                                        break;
                                    }
                                }
                                _ => {
                                    debug!("Unforwarded Telemetry event: {:?}", event);
                                }
                            }
                    }
                }
                _ = flush_interval.tick() => {
                    tokio::join!(
                        logs_flusher.flush(),
                        metrics_flusher.flush(),
                        trace_flusher.manual_flush(),
                        stats_flusher.manual_flush()
                    );
                    if !flush_control.should_flush_end() {
                        break;
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
                trace_flusher.manual_flush(),
                stats_flusher.manual_flush()
            );
            return Ok(());
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
