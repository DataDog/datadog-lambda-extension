use std::error::Error;
use std::sync::Arc;

use serde::Serialize;

use crate::config;
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
    datadog_config: Arc<config::Config>,
}

impl Processor {
    pub fn new(function_arn: String, datadog_config: Arc<config::Config>) -> Processor {
        Processor {
            function_arn,
            datadog_config,
        }
    }

    pub fn process(&self, event: TelemetryEvent) -> Result<IntakeLog, Box<dyn Error>> {
        let service = self
            .datadog_config
            .service
            .clone()
            .unwrap_or("".to_string());
        let tags = self.datadog_config.tags.clone().unwrap_or("".to_string());
        let mut log = IntakeLog {
            hostname: self.function_arn.clone(),
            source: LOGS_SOURCE.to_string(),
            service,
            tags,
            message: LambdaMessage {
                message: "placeholder-message".to_string(),
                lambda: Lambda {
                    arn: self.function_arn.clone(),
                    request_id: "placeholder-request-id".to_string(), // TODO: replace with incoming request id
                },
                timestamp: event.time.timestamp_millis(),
                status: "placeholder-status".to_string(),
            },
        };
        match event.record {
            TelemetryRecord::Function(v) | TelemetryRecord::Extension(v) => {
                log.message.status = "info".to_string();
                log.message.message = v;

                Ok(log)
            }
            TelemetryRecord::PlatformInitStart {
                runtime_version,
                runtime_version_arn,
                .. // TODO: check if we could do something with this metrics: `initialization_type` and `phase`
            } => {
                let rv = runtime_version.unwrap_or("?".to_string()); // TODO: check what does containers display
                let rv_arn = runtime_version_arn.unwrap_or("?".to_string()); // TODO: check what do containers display
                log.message.message = format!("INIT_START Runtime Version: {} Runtime Version ARN: {}", rv, rv_arn);
                log.message.status = "info".to_string();

                Ok(log)
            },
            TelemetryRecord::PlatformStart {
                request_id,
                version,
            } => {
                let version = version.unwrap_or("$LATEST".to_string());
                log.message.message =
                    format!("START RequestId: {} Version: {}", request_id, version);
                log.message.lambda.request_id = request_id;
                log.message.status = "info".to_string();

                Ok(log)
            },
            TelemetryRecord::PlatformRuntimeDone { request_id , .. } => {  // TODO: check what to do with rest of the fields
                log.message.message = format!("END RequestId: {}", request_id);
                log.message.lambda.request_id = request_id;
                log.message.status = "info".to_string();
                Ok(log)
            },
            TelemetryRecord::PlatformReport { request_id, metrics, .. } => { // TODO: check what to do with rest of the fields
                let mut message = format!(
                    "REPORT RequestId: {} Duration: {} ms Billed Duration: {} ms Memory Size: {} MB Max Memory Used: {} MB",
                    request_id,
                    metrics.duration_ms,
                    metrics.billed_duration_ms,
                    metrics.memory_size_mb,
                    metrics.max_memory_used_mb,
                );

                let init_duration_ms = metrics.init_duration_ms;
                if let Some(init_duration_ms) = init_duration_ms {
                    message = format!("{} Init Duration: {} ms", message, init_duration_ms)
                }

                log.message.message = message;
                log.message.lambda.request_id = request_id;
                log.message.status = "info".to_string();

                Ok(log)
            },
            _ => Err("Unsupported event type".into()),
        }
    }
}
