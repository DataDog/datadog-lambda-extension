use reqwest::Response;
use serde_json;
use std::error::Error;
use tracing::{debug, error};

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

    pub async fn subscribe(&self) -> Result<Response, Box<dyn Error>> {
        let url = base_url(TELEMETRY_SUBSCRIPTION_ROUTE)?;
        let resp = reqwest::Client::builder()
            .use_rustls_tls()
            .no_proxy()
            .build()
            .map_err(|e| {
                error!("Error building reqwest client: {:?}", e);
                e
            })?
            .put(&url)
            .header(EXTENSION_ID_HEADER, &self.extension_id)
            .json(&serde_json::json!({
                "schemaVersion": "2022-12-13",
                "destination": {
                    "protocol": "HTTP",
                    "URI": format!("http://sandbox:{}/", &self.port),
                },
                "types": ["extension", "function", "platform"],
                "buffering": { // TODO: re evaluate using default values
                    "maxItems": 1000,
                    "maxBytes": 256 * 1024,
                    "timeoutMs": 25
                }
            }))
            .send()
            .await;

        debug!("Subscribed to telemetry: {:?}", resp);
        Ok(resp?)
    }
}
