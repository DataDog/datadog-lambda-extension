use std::{env, error::Error};

use tracing::{debug, error};

use crate::logs::processor::IntakeLog;

const LOGS_API_INTAKE: &str = "https://http-intake.logs.datadoghq.com/api/v2/logs";

pub struct DdApi {
    api_key: String,
    ureq_agent: ureq::Agent,
}

impl DdApi {
    pub fn new() -> Self {
        DdApi {
            api_key: env::var("DD_API_KEY").expect("unable to read envvars, catastrophic failure"),
            ureq_agent: ureq::AgentBuilder::new().build(),
        }
    }

    pub fn send(&self, logs: &Vec<IntakeLog>) -> Result<(), Box<dyn Error>> {
        let api_key = self.api_key.clone();

        if !logs.is_empty() {
            let resp: Result<ureq::Response, ureq::Error> = self
                .ureq_agent
                .post(LOGS_API_INTAKE)
                .set("DD-API-KEY", &api_key)
                .set("DD-PROTOCOL", "agent-json")
                .set("Content-Type", "application/json")
                .send_json(logs);

            match resp {
                Ok(resp) => {
                    if resp.status() != 200 {
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
