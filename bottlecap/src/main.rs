#![deny(clippy::all)]
mod config;
mod lifecycle_handler;
mod logger;
mod logs_handler;

use std::{os::unix::process::CommandExt, path::Path, process::Command};

use lambda_extension::{service_fn, Error, Extension, SharedService};

use lifecycle_handler::lifecycle_handler;
use logger::SimpleLogger;
use logs_handler::logs_handler;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // First load the configuration
    let lambda_directory = std::env::var("LAMBDA_TASK_ROOT").unwrap_or("".to_string());
    let config = match config::get_config(Path::new(&lambda_directory)) {
        Ok(config) => {
            println!("ASTUYVE config passed let's go");
            config
        },
        Err(e) => {
            println!("ASTUYVE config failed, booting into agent");
            log::error!("Error loading configuration: {:?}", e);
            let err = Command::new("/opt/datadog-agent-go").exec();
            panic!("Error starting the extension: {:?}", err);
        }
    };
    SimpleLogger::init(config.log_level).expect("Error initializing logger");

    let logs_processor = SharedService::new(service_fn(logs_handler));
    let lifecycle_processor = service_fn(lifecycle_handler);

    println!("Bottlecap is registering");
    Extension::new()
        .with_logs_processor(logs_processor)
        .with_events_processor(lifecycle_processor)
        .run()
        .await?;

    Ok(())
}
