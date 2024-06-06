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

use decrypt::resolve_secrets;
use lifecycle::flush_control::FlushControl;
use std::collections::hash_map;
use telemetry::listener::TelemetryListenerConfig;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

use bottlecap::{
    base_url, config,
    event_bus::bus::EventBus,
    events::Event,
    lifecycle, logger,
    logs::agent::LogsAgent,
    logs::flusher::Flusher as LogsFlusher,
    metrics::{
        aggregator as metrics_aggregator, constants,
        dogstatsd::{DogStatsD, DogStatsDConfig},
        enhanced::lambda::Lambda as enhanced_metrics,
        flusher::Flusher as MetricsFlusher,
    },
    secrets::decrypt,
    tags::{lambda, provider},
    telemetry::{
        self, client::TelemetryApiClient, events::Status, events::TelemetryRecord,
        listener::TelemetryListener,
    },
    DOGSTATSD_PORT, EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES, EXTENSION_HOST,
    EXTENSION_ID_HEADER, EXTENSION_NAME, EXTENSION_NAME_HEADER, EXTENSION_ROUTE,
    LAMBDA_RUNTIME_SLUG, TELEMETRY_PORT,
};

use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::io::Error;
use std::io::Result;
use std::sync::{Arc, Mutex};
use std::{os::unix::process::CommandExt, path::Path, process::Command};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterResponse {
    // Skip deserialize because this field is not available in the response
    // body, but as a header. Header is extracted and set manually.
    #[serde(skip_deserializing)]
    extension_id: String,
    account_id: String,
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

#[allow(clippy::unwrap_used)]
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

    assert!(resp.status() == 200, "Unable to register extension");

    let extension_id = resp
        .headers()
        .get(EXTENSION_ID_HEADER)
        .unwrap()
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

#[allow(clippy::too_many_lines)]
#[tokio::main]
async fn main() -> Result<()> {
    // First load the configuration
    let lambda_directory = env::var("LAMBDA_TASK_ROOT").unwrap_or_else(|_| "/var/task".to_string());
    let env_config = match config::get_config(Path::new(&lambda_directory)) {
        Ok(config) => config,
        Err(e) => {
            // NOTE we must print here as the logging subsystem is not enabled yet.
            println!("Error loading configuration: {e:?}");
            let err = Command::new("/opt/datadog-agent-go").exec();
            panic!("Error starting the extension: {err:?}");
        }
    };

    // Bridge any `log` logs into the tracing subsystem. Note this is a global
    // registration.
    tracing_log::LogTracer::builder()
        .with_max_level(env_config.log_level.as_level_filter())
        .init()
        .expect("failed to set up log bridge");

    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(
            EnvFilter::try_new(env_config.log_level)
                .expect("could not parse log level in configuration"),
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

    info!("logging subsystem enabled");
    let client = reqwest::Client::new();

    let r = register(&client)
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    let config = match resolve_secrets(env_config).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            panic!("Error resolving key: {e}");
        }
    };

    let region = env::var("AWS_REGION").expect("could not read AWS_REGION");
    let function_name =
        env::var("AWS_LAMBDA_FUNCTION_NAME").expect("could not read AWS_LAMBDA_FUNCTION_NAME");
    let function_arn = build_function_arn(&r.account_id, &region, &function_name);
    let metadata_hash = hash_map::HashMap::from([(
        lambda::tags::FUNCTION_ARN_KEY.to_string(),
        function_arn.clone(),
    )]);
    let tags_provider = Arc::new(provider::Provider::new(
        Arc::clone(&config),
        LAMBDA_RUNTIME_SLUG.to_string(),
        &metadata_hash,
    ));

    let mut event_bus = EventBus::run();
    let mut logs_agent = LogsAgent::new(
        Arc::clone(&tags_provider),
        Arc::clone(&config),
        event_bus.get_sender_copy(),
    );
    let logs_agent_channel = logs_agent.get_sender_copy();
    let logs_flusher = LogsFlusher::new(Arc::clone(&config), Arc::clone(&logs_agent.aggregator));
    tokio::spawn(async move {
        logs_agent.spin().await;
    });
    let metrics_aggr = Arc::new(Mutex::new(
        metrics_aggregator::Aggregator::<{ constants::CONTEXTS }>::new(tags_provider.clone())
            .expect("failed to create aggregator"),
    ));
    let dogstatsd_config = DogStatsDConfig {
        host: EXTENSION_HOST.to_string(),
        port: DOGSTATSD_PORT,
        datadog_config: Arc::clone(&config),
        aggregator: Arc::clone(&metrics_aggr),
        tags_provider: Arc::clone(&tags_provider),
    };
    let lambda_enhanced_metrics = enhanced_metrics::new(Arc::clone(&metrics_aggr));
    let dogstats_cancel_token = tokio_util::sync::CancellationToken::new();
    let dogstats_client = DogStatsD::new(
        &dogstatsd_config,
        event_bus.get_sender_copy(),
        dogstats_cancel_token.clone(),
    )
    .await;

    let mut statsd_flusher =
        MetricsFlusher::new(Arc::clone(&config), Arc::clone(&dogstats_client.aggregator));

    tokio::spawn(async move {
        dogstats_client.spin().await;
    });
    let telemetry_listener_config = TelemetryListenerConfig {
        host: EXTENSION_HOST.to_string(),
        port: TELEMETRY_PORT,
    };
    let telemetry_listener_cancel_token = tokio_util::sync::CancellationToken::new();
    let telemetry_listener = TelemetryListener::new(
        &telemetry_listener_config,
        logs_agent_channel,
        telemetry_listener_cancel_token.clone(),
    )
    .await
    .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    tokio::spawn(async move {
        telemetry_listener.spin().await;
    });

    let telemetry_client = TelemetryApiClient::new(r.extension_id.to_string(), TELEMETRY_PORT);
    telemetry_client
        .subscribe()
        .await
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    let mut flush_control = FlushControl::new(config.serverless_flush_strategy);
    let mut shutdown = false;

    loop {
        let evt = next_event(&client, &r.extension_id).await;
        match evt {
            Ok(NextEventResponse::Invoke {
                request_id,
                deadline_ms,
                invoked_function_arn,
            }) => {
                info!(
                    "[bottlecap] Invoke event {}; deadline: {}, invoked_function_arn: {}",
                    request_id, deadline_ms, invoked_function_arn
                );
                if let Err(e) = lambda_enhanced_metrics.increment_invocation_metric() {
                    error!("Failed to increment invocation metric: {e:?}");
                }
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
        if flush_control.should_flush() || shutdown {
            loop {
                let received = event_bus.rx.recv().await;
                if let Some(event) = received {
                    match event {
                        Event::Metric(event) => {
                            error!("Metric event: {:?}", event);
                        }
                        Event::Telemetry(event) => match event.record {
                            TelemetryRecord::PlatformInitReport {
                                initialization_type,
                                phase,
                                metrics,
                            } => {
                                error!("Platform init report for initialization_type: {:?} with phase: {:?} and metrics: {:?}", initialization_type, phase, metrics);
                                let _ = lambda_enhanced_metrics
                                    .set_init_duration_metric(metrics.duration_ms);
                            }
                            TelemetryRecord::PlatformRuntimeDone {
                                request_id, status, ..
                            } => {
                                if status != Status::Success {
                                    if let Err(e) =
                                        lambda_enhanced_metrics.increment_errors_metric()
                                    {
                                        error!("Failed to increment error metric: {e:?}");
                                    }
                                    if status == Status::Timeout {
                                        if let Err(e) =
                                            lambda_enhanced_metrics.increment_timeout_metric()
                                        {
                                            error!("Failed to increment timeout metric: {e:?}");
                                        }
                                    }
                                }
                                debug!(
                                    "Runtime done for request_id: {:?} with status: {:?}",
                                    request_id, status
                                );
                                logs_flusher.flush().await;
                                statsd_flusher.flush().await;
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
                                lambda_enhanced_metrics.set_report_log_metrics(&metrics);
                                if shutdown {
                                    break;
                                }
                            }
                            _ => {
                                debug!("Unforwarded Telemetry event: {:?}", event);
                            }
                        },
                    }
                } else {
                    error!("could not get the event");
                }
            }
        }

        if shutdown {
            dogstats_cancel_token.cancel();
            telemetry_listener_cancel_token.cancel();
            logs_flusher.flush().await;
            statsd_flusher.flush().await;
            return Ok(());
        }
    }
}
