use std::time::Duration;

use bottlecap::{
    config::aws::AwsConfig,
    extension::{self},
};
use recorder::{listener, logger};
use reqwest::Client;
use tokio::time::Instant;
use tracing::debug;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    enable_logging_subsystem();
    let now = Instant::now();
    let client = Client::builder().build().expect("Failed to build client");
    let aws_config = AwsConfig::from_env(now);

    // Start the listener server in a separate task
    tokio::spawn(async {
        if let Err(e) = listener::start_listener().await {
            eprintln!("Failed to start listener: {}", e);
        }
    });

    let r = extension::register(&client, &aws_config.runtime_api, "datadog-recorder")
        .await
        .expect("Failed to register extension");
    debug!("Extension registered with id: {}", &r.extension_id);

    let next_event_response: Result<extension::NextEventResponse, extension::ExtensionError> =
        extension::next_event(&client, &aws_config.runtime_api, &r.extension_id).await;
    handle_next_invocation(next_event_response);

    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        debug!("Calling next event");
        let next_event_response =
            extension::next_event(&client, &aws_config.runtime_api, &r.extension_id).await;
        let current_event = handle_next_invocation(next_event_response);

        if let extension::NextEventResponse::Shutdown { .. } = current_event {
            debug!("Shutdown event received");
            return Ok(());
        }
    }
}

fn enable_logging_subsystem() {
    let env_filter = "h2=off,hyper=off,reqwest=off,rustls=off,DEBUG";
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

fn handle_next_invocation(
    next_response: Result<extension::NextEventResponse, extension::ExtensionError>,
) -> extension::NextEventResponse {
    match next_response {
        Ok(extension::NextEventResponse::Invoke {
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
        }
        Ok(extension::NextEventResponse::Shutdown {
            ref shutdown_reason,
            deadline_ms,
        }) => {
            println!("Exiting: {shutdown_reason}, deadline: {deadline_ms}");
        }
        Err(ref err) => {
            eprintln!("Error: {err:?}");
            println!("Exiting");
        }
    }
    next_response.unwrap_or(extension::NextEventResponse::Shutdown {
        shutdown_reason: "panic".into(),
        deadline_ms: 0,
    })
}
