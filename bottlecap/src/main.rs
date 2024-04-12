#![deny(clippy::all)]
mod config;
mod logger;

use std::collections::HashMap;
use std::env;
use std::io::Error;
use std::io::Result;

use std::{os::unix::process::CommandExt, path::Path, process::Command};

use logger::SimpleLogger;
use serde::Deserialize;

const EXTENSION_NAME: &str = "datadog-agent";
const EXTENSION_NAME_HEADER: &str = "Lambda-Extension-Name";
const EXTENSION_ID_HEADER: &str = "Lambda-Extension-Identifier";

struct RegisterResponse {
    pub extension_id: String,
}

fn base_url() -> Result<String> {
    Ok(format!(
        "http://{}/2020-01-01/extension",
        env::var("AWS_LAMBDA_RUNTIME_API")
            .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
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
    let base_url =
        base_url().map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
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
    let base_url =
        base_url().map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    map.insert("events", vec!["INVOKE", "SHUTDOWN"]);
    let url = format!("{}/register", base_url);

    let resp = ureq::post(&url)
        .set(EXTENSION_NAME_HEADER, EXTENSION_NAME)
        .send_json(ureq::json!(map))
        .map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    if resp.status() != 200 {
        panic!("Unable to register extension")
    }

    let ext_id = resp.header(EXTENSION_ID_HEADER).unwrap_or_default();
    Ok(RegisterResponse {
        extension_id: ext_id.to_string(),
    })
}

fn main() -> Result<()> {
    // First load the configuration
    let lambda_directory = std::env::var("LAMBDA_TASK_ROOT").unwrap_or("".to_string());
    let config = match config::get_config(Path::new(&lambda_directory)) {
        Ok(config) => config,
        Err(e) => {
            log::error!("Error loading configuration: {:?}", e);
            let err = Command::new("/opt/datadog-agent-go").exec();
            panic!("Error starting the extension: {:?}", err);
        }
    };
    SimpleLogger::init(config.log_level).expect("Error initializing logger");

    let r = register().map_err(|e| Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

    loop {
        let evt = next_event(&r.extension_id);
        match evt {
            Ok(NextEventResponse::Invoke {
                request_id,
                deadline_ms,
                invoked_function_arn,
            }) => {
                println!(
                    "[bottlecap] Invoke event {}; deadline: {}, invoked_function_arn: {}",
                    request_id, deadline_ms, invoked_function_arn
                );
            }
            Ok(NextEventResponse::Shutdown {
                shutdown_reason,
                deadline_ms,
            }) => {
                println!("Exiting: {}, deadline: {}", shutdown_reason, deadline_ms);
                return Ok(());
            }
            Err(err) => {
                eprintln!("Error: {:?}", err);
                println!("Exiting");
                return Err(err);
            }
        }
    }
}
