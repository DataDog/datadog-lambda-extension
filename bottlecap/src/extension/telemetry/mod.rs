use reqwest::{Client, Response};
use tracing::debug;

use crate::extension::{EXTENSION_ID_HEADER, base_url};

pub mod events;
pub mod listener;

pub const TELEMETRY_SUBSCRIPTION_ROUTE: &str = "2022-07-01/telemetry";

// Technically, we could use the same schema version for both modes,
// but we'll keep them separate for now to avoid regressions. The
// naming is just to differentiate which one contains Elevator-specific
// events.
pub const ELEVATOR_SCHEMA_VERSION: &str = "2025-01-29";
pub const ON_DEMAND_SCHEMA_VERSION: &str = "2022-12-13";
// todo(astuyve) should be 8124 on /lambda/logs but
// telemetry is implemented on a raw socket now and
// does not multiplex routes on the same port.
pub const TELEMETRY_PORT: u16 = 8999;

const PLATFORM_ONLY_EVENTS: &[&str] = &["platform"];
const ALL_EVENTS: &[&str] = &["platform", "extension", "function"];

/// Error conditions that can arise from extension operations
#[derive(thiserror::Error, Debug)]
pub enum ExtensionSubscriptionError {
    #[error("Subscription request failed: {0}")]
    HttpError(#[from] reqwest::Error),
}

fn get_subscription_event_types(logs_enabled: bool) -> Vec<&'static str> {
    (if logs_enabled {
        ALL_EVENTS
    } else {
        PLATFORM_ONLY_EVENTS
    })
    .to_vec()
}

pub async fn subscribe(
    client: &Client,
    runtime_api: &str,
    extension_id: &str,
    destination_port: u16,
    logs_enabled: bool,
    elevator_mode: bool,
) -> Result<Response, ExtensionSubscriptionError> {
    let schema_version = if elevator_mode {
        ELEVATOR_SCHEMA_VERSION
    } else {
        ON_DEMAND_SCHEMA_VERSION
    };
    let url = base_url(TELEMETRY_SUBSCRIPTION_ROUTE, runtime_api);
    let response = client
        .put(&url)
        .header(EXTENSION_ID_HEADER, extension_id)
        .json(&serde_json::json!({
            "schemaVersion": schema_version,
            "destination": {
                "protocol": "HTTP",
                "URI": format!("http://sandbox:{}/", destination_port),
            },
            "types": get_subscription_event_types(logs_enabled),
            "buffering": { // TODO: re evaluate using default values
                "maxItems": 1000,
                "maxBytes": 256 * 1024,
                "timeoutMs": 25
            }
        }))
        .send()
        .await?;

    debug!("EXTENSION | Subscribed to Telemetry API: {:?}", response);
    Ok(response)
}
