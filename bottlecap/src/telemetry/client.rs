use tracing::{debug, error};

use crate::{base_url, EXTENSION_ID_HEADER, TELEMETRY_SUBSCRIPTION_ROUTE};

pub struct TelemetryApiClient {
    pub extension_id: String,
    pub port: u16,
}

impl TelemetryApiClient {
    pub fn new(extension_id: String, port: u16) -> Self {
        TelemetryApiClient {
            extension_id,
            port
        }
    }

    pub fn subscribe(&self) {
        let url = base_url(TELEMETRY_SUBSCRIPTION_ROUTE).expect("Couldn't get base URL"); // TODO: handle error
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
                "timeoutMs": 1000
            }
        });


        let resp = ureq::put(&url)
            .set("Content-Type", "application/json")
            .set(EXTENSION_ID_HEADER, &self.extension_id)
            .send_json(data);
    
        match resp { // TODO: handle error
            Result::Ok(_) => {
                debug!("Subscribed to the Telemetry API");
            },
            Err(e) => {
                error!("Error subscribing to the Telemetry API: {}", e);
            }
        }
    }
}
