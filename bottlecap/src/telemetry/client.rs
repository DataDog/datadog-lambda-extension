use std::error::Error;
use tracing::debug;

use crate::{base_url, EXTENSION_ID_HEADER, TELEMETRY_SUBSCRIPTION_ROUTE};

#[allow(clippy::module_name_repetitions)]
pub struct TelemetryApiClient {
    pub extension_id: String,
    pub port: u16,
}

impl TelemetryApiClient {
    #[must_use]
    pub fn new(extension_id: String, port: u16) -> Self {
        TelemetryApiClient { extension_id, port }
    }

    pub fn subscribe(&self) -> Result<ureq::Response, Box<dyn Error>> {
        let url = base_url(TELEMETRY_SUBSCRIPTION_ROUTE)?;
        let data = ureq::json!({
            "schemaVersion": "2022-12-13",
            "destination": {
                "protocol": "HTTP",
                "URI": format!("http://sandbox:{}/", &self.port),
            },
            "types": ["function", "platform"],
            "buffering": { // TODO: re evaluate using default values
                "maxItems": 1000,
                "maxBytes": 256 * 1024,
                "timeoutMs": 25
            }
        });

        let resp = ureq::put(&url)
            .set("Content-Type", "application/json")
            .set(EXTENSION_ID_HEADER, &self.extension_id)
            .send_json(data);

        debug!("subscribed to telemetry: {:?}", resp);
        Ok(resp?)
    }
}
