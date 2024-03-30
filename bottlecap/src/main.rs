#![deny(clippy::all)]
mod config;
mod lifecycle_handler;
mod logger;
mod logs_handler;

use std::{os::unix::process::CommandExt, process::Command};

use lambda_extension::{service_fn, Error, Extension, SharedService};

use lifecycle_handler::lifecycle_handler;
use logger::SimpleLogger;
use logs_handler::logs_handler;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // First load the configuration
    let config = match config::get_config() {
        Ok(config) => config,
        Err(e) => {
            log::error!("Error loading configuration: {:?}", e);
            let err = Command::new("../bin/datadog-lambda-extension").exec();
            panic!("Error starting the extension: {:?}", err);
        }
    };
    SimpleLogger::init(config.log_level).expect("Error initializing logger");

    let logs_processor = SharedService::new(service_fn(logs_handler));
    let lifecycle_processor = service_fn(lifecycle_handler);

    Extension::new()
        .with_logs_processor(logs_processor)
        .with_events_processor(lifecycle_processor)
        .run()
        .await?;

    Ok(())
}
