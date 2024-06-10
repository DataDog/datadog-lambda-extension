use log::warn;
use reqwest;
use std::error::Error;
use tracing::{debug, error};

pub struct Api {
    api_key: Option<String>,
    site: String,
    client: reqwest::Client,
}

impl Api {
    #[must_use]
    pub fn new(api_key: Option<String>, site: String) -> Self {
        Api {
            api_key,
            site,
            client: reqwest::Client::new(),
        }
    }

    pub async fn send(&self, data: Vec<u8>) -> Result<(), Box<dyn Error>> {
        let url = format!("https://http-intake.logs.{}/api/v2/logs", &self.site);

        match &self.api_key {
            Some(api_key) => {
                // It could be an empty JSON array: []
                if data.len() > 2 {
                    let resp: Result<reqwest::Response, reqwest::Error> = self
                        .client
                        .post(&url)
                        .header("DD-API-KEY", api_key)
                        .header("DD-PROTOCOL", "agent-json")
                        .header("Content-Type", "application/json")
                        .body(data)
                        .send()
                        .await;

                    match resp {
                        Ok(resp) => {
                            if resp.status() != 202 {
                                debug!("Failed to send logs to datadog: {}", resp.status());
                            }
                        }
                        Err(e) => {
                            error!("Failed to send logs to datadog: {}", e);
                        }
                    }
                }
                Ok(())
            }
            None => {
                warn!("API key is not set, skipping sending logs to Datadog");
                Ok(())
            }
        }
    }
}
