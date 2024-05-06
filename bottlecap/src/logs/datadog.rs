use std::error::Error;

use tracing::{debug, error};

use crate::logs::processor::IntakeLog;

pub struct Api {
    api_key: String,
    site: String,
    ureq_agent: ureq::Agent,
}

impl Api {
    pub fn new(api_key: String, site: String) -> Self {
        Api {
            api_key,
            site,
            ureq_agent: ureq::AgentBuilder::new().build(),
        }
    }

    pub fn send(&self, logs: &Vec<IntakeLog>) -> Result<(), Box<dyn Error>> {
        let url = format!("https://http-intake.logs.{}/api/v2/logs", &self.site);

        if !logs.is_empty() {
            let resp: Result<ureq::Response, ureq::Error> = self
                .ureq_agent
                .post(&url)
                .set("DD-API-KEY", &self.api_key)
                .set("DD-PROTOCOL", "agent-json")
                .set("Content-Type", "application/json")
                .send_json(logs);

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
}
