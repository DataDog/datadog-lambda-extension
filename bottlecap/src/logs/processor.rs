use std::error::Error;

use serde::Serialize;

use crate::telemetry::events::{TelemetryEvent, TelemetryRecord};

const LOGS_SOURCE: &str = "lambda";

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Lambda {
    pub arn: String,
    pub request_id: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct LambdaMessage {
    pub message: String,
    pub lambda: Lambda,
    pub timestamp: i64,
    pub status: String,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct IntakeLog {
    pub message: LambdaMessage,
    pub hostname: String,
    pub service: String,
    #[serde(rename(serialize = "ddtags"))]
    pub tags: String,
    #[serde(rename(serialize = "ddsource"))]
    pub source: String,
}

#[derive(Clone)]
pub struct Processor {
    function_arn: String,
}

impl Processor {
    pub fn new(function_arn: String) -> Processor {
        Processor { function_arn }
    }

    pub fn process(&self, event: TelemetryEvent) -> Result<IntakeLog, Box<dyn Error>> {
        match event.record {
            TelemetryRecord::PlatformStart {
                request_id,
                version,
            } => {
                let version = version.unwrap_or("$LATEST".to_string());
                Ok(IntakeLog {
                    message: LambdaMessage {
                        message: format!("START RequestId: {} Version: {}", request_id, version),
                        lambda: Lambda {
                            arn: self.function_arn.clone(),
                            request_id,
                        },
                        timestamp: event.time.timestamp_millis(),
                        status: "info".to_string(),
                    },
                    hostname: self.function_arn.clone(),
                    service: "TODO-add-service".to_string(),
                    tags: "todo:add-tags".to_string(),
                    source: LOGS_SOURCE.to_string(),
                })
            }
            _ => Err("Unsupported event type".into()),
        }
    }
}
