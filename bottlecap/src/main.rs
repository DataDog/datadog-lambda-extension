#![deny(clippy::all)]
mod config;
mod event_bus;
mod lifecycle;
mod logs;
mod metrics;
mod telemetry;
use lifecycle::flush_control::FlushControl;
use telemetry::listener::TelemetryListenerConfig;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

mod events;
mod logger;

use crate::event_bus::bus::EventBus;
use crate::events::Event;
use crate::logs::agent::LogsAgent;
use crate::metrics::dogstatsd::{DogStatsD, DogStatsDConfig};
use crate::telemetry::events::TelemetryRecord;
use crate::telemetry::{client::TelemetryApiClient, listener::TelemetryListener};

use std::collections::HashMap;
use std::env;
use std::io::Error;
use std::io::Result;

use std::sync::Arc;
use std::{os::unix::process::CommandExt, path::Path, process::Command};

use serde::Deserialize;

const EXTENSION_HOST: &str = "0.0.0.0";
const EXTENSION_NAME: &str = "datadog-agent";
const EXTENSION_FEATURES: &str = "accountId";
const EXTENSION_NAME_HEADER: &str = "Lambda-Extension-Name";
const EXTENSION_ID_HEADER: &str = "Lambda-Extension-Identifier";
const EXTENSION_ACCEPT_FEATURE_HEADER: &str = "Lambda-Extension-Accept-Feature";
const EXTENSION_ROUTE: &str = "2020-01-01/extension";

// todo: make sure we can override those with environment variables
const DOGSTATSD_PORT: u16 = 8185;

const TELEMETRY_SUBSCRIPTION_ROUTE: &str = "2022-07-01/telemetry";
const TELEMETRY_PORT: u16 = 8124;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegisterResponse {
    // Skip deserialize because this field is not available in the response
    // body, but as a header. Header is extracted and set manually.
    #[serde(skip_deserializing)]
    pub extension_id: String,
    pub account_id: String,
}

fn base_url(route: &str) -> Result<String> {
    Ok(format!(
        "http://{}/{}",
        env::var("AWS_LAMBDA_RUNTIME_API")
            .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?,
        route
    ))
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

fn next_event(ext_id: &str) -> Result<NextEventResponse> {
    let base_url = base_url(EXTENSION_ROUTE)
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let url = format!("{}/event/next", base_url);
    ureq::get(&url)
        .set(EXTENSION_ID_HEADER, ext_id)
        .call()
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
        .into_json()
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

fn register() -> Result<RegisterResponse> {
    let mut map = HashMap::new();
    let base_url = base_url(EXTENSION_ROUTE)
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    map.insert("events", vec!["INVOKE", "SHUTDOWN"]);
    let url = format!("{}/register", base_url);

    let resp = ureq::post(&url)
        .set(EXTENSION_NAME_HEADER, EXTENSION_NAME)
        .set(EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES)
        .send_json(ureq::json!(map))
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    if resp.status() != 200 {
        panic!("Unable to register extension")
    }

    let extension_id = resp
        .header(EXTENSION_ID_HEADER)
        .unwrap_or_default()
        .to_string();
    let mut register_response = resp.into_json::<RegisterResponse>()?;

    // Set manually since it's not part of the response body
    register_response.extension_id = extension_id;

    Ok(register_response)
}

fn build_function_arn(account_id: &str, region: &str, function_name: &str) -> String {
    format!(
        "arn:aws:lambda:{}:{}:function:{}",
        region, account_id, function_name
    )
}

fn main() -> Result<()> {
    // First load the configuration
    let lambda_directory = env::var("LAMBDA_TASK_ROOT")
        .expect("unable to read environment variable: LAMBDA_TASK_ROOT");
    let config = match config::get_config(Path::new(&lambda_directory)) {
        Ok(config) => Arc::new(config),
        Err(e) => {
            // NOTE we must print here as the logging subsystem is not enabled yet.
            println!("Error loading configuration: {:?}", e);
            let err = Command::new("/opt/datadog-agent-go").exec();
            panic!("Error starting the extension: {:?}", err);
        }
    };

    // Bridge any `log` logs into the tracing subsystem. Note this is a global
    // registration.
    tracing_log::LogTracer::builder()
        .with_max_level(config.log_level.as_level_filter())
        .init()
        .expect("failed to set up log bridge");

    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_env_filter(
            EnvFilter::try_new(config.log_level)
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

    let r = register().map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    let region = env::var("AWS_REGION").unwrap();
    let function_name = env::var("AWS_LAMBDA_FUNCTION_NAME").unwrap();
    let function_arn = build_function_arn(&r.account_id, &region, &function_name);

    let logs_agent = LogsAgent::run(&function_arn, Arc::clone(&config));
    let event_bus = EventBus::run();
    let dogstatsd_config = DogStatsDConfig {
        host: EXTENSION_HOST.to_string(),
        port: DOGSTATSD_PORT,
        datadog_config: Arc::clone(&config),
    };
    let mut dogstats_client = DogStatsD::run(&dogstatsd_config, event_bus.get_sender_copy());

    let telemetry_listener_config = TelemetryListenerConfig {
        host: EXTENSION_HOST.to_string(),
        port: TELEMETRY_PORT,
    };
    let telemetry_listener =
        TelemetryListener::run(&telemetry_listener_config, event_bus.get_sender_copy())
            .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let telemetry_client = TelemetryApiClient::new(r.extension_id.to_string(), TELEMETRY_PORT);
    telemetry_client
        .subscribe()
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    let mut flush_control = FlushControl::new(config.serverless_flush_strategy);
    let mut shutdown = false;

    loop {
        let evt = next_event(&r.extension_id);
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
            }
            Ok(NextEventResponse::Shutdown {
                shutdown_reason,
                deadline_ms,
            }) => {
                println!("Exiting: {}, deadline: {}", shutdown_reason, deadline_ms);
                shutdown = true;
            }
            Err(err) => {
                eprintln!("Error: {:?}", err);
                println!("Exiting");
                return Err(err);
            }
        }
        // Block until we get something from the telemetry API
        // Check if flush logic says we should block and flush or not
        if flush_control.should_flush() || shutdown {
            loop {
                let received = event_bus.rx.recv();
                if let Ok(event) = received {
                    match event {
                        Event::Metric(event) => {
                            debug!("Metric event: {:?}", event);
                        }
                        Event::Telemetry(event) => {
                            logs_agent.send_event(event.clone());
                            match event.record {
                                TelemetryRecord::PlatformInitReport {
                                    initialization_type,
                                    phase,
                                    metrics,
                                } => {
                                    debug!("Platform init report for initialization_type: {:?} with phase: {:?} and metrics: {:?}", initialization_type, phase, metrics);
                                    // write this straight to metrics aggr
                                    // write this straight to logs aggr
                                    // write this straight to trace aggr
                                }
                                TelemetryRecord::PlatformRuntimeDone {
                                    request_id, status, ..
                                } => {
                                    debug!(
                                        "Runtime done for request_id: {:?} with status: {:?}",
                                        request_id, status
                                    );
                                    debug!("FLUSHING ALL");
                                    logs_agent.flush();
                                    dogstats_client.flush();
                                    debug!("CALLING FOR NEXT EVENT");
                                    break;
                                }
                                TelemetryRecord::PlatformReport {
                                    request_id, status, ..
                                } => {
                                    debug!(
                                        "Platform report for request_id: {:?} with status: {:?}",
                                        request_id, status
                                    );
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
                } else {
                    error!("could not get the event");
                }
            }
        }

        if shutdown {
            logs_agent.shutdown();
            dogstats_client.shutdown();
            telemetry_listener.shutdown();
            return Ok(());
        }
    }
}
