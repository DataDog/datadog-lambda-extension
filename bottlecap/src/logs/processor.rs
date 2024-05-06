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
                // Default status
                status: "info".to_string(),
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

                Ok(log)
            },
            TelemetryRecord::PlatformRuntimeDone { request_id , .. } => {  // TODO: check what to do with rest of the fields
                log.message.message = format!("END RequestId: {}", request_id);
                log.message.lambda.request_id = request_id;
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

                Ok(log)
            },
            // TODO: PlatformInitRuntimeDone
            // TODO: PlatformInitReport
            // TODO: PlatformExtension
            // TODO: PlatformTelemetrySubscription
            // TODO: PlatformLogsDropped
            _ => Err("Unsupported event type".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::telemetry::events::{
        InitPhase, InitType, ReportMetrics, RuntimeDoneMetrics, Status,
    };

    use super::*;

    use chrono::{TimeZone, Utc};

    macro_rules! process_tests {
        ($($name:ident: $value:expr,)*) => {
            $(
                #[test]
                fn $name() {
                    let (input, expected): (&TelemetryEvent, IntakeLog) = $value;

                    let processor = Processor::new(
                        "arn".to_string(),
                        Arc::new(config::Config {
                            service: Some("test-service".to_string()),
                            tags: Some("test:tag,env:test".to_string()),
                            ..config::Config::default()})
                    );
                    let result = processor.process(input.clone()).unwrap();

                    assert_eq!(result, expected);
                }
            )*
        }
    }

    process_tests! {
        // function
        function: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::Function("test-function".to_string())
            },
            IntakeLog {
                hostname: "arn".to_string(),
                source: "lambda".to_string(),
                service: "test-service".to_string(),
                tags: "test:tag,env:test".to_string(),
                message: LambdaMessage {
                    message: "test-function".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: "placeholder-request-id".to_string(),
                    },
                    timestamp: 1673061827000,
                    status: "info".to_string(),
                },
            }
        ),

        // extension
        extension: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::Extension("test-extension".to_string())
            },
            IntakeLog {
                hostname: "arn".to_string(),
                source: "lambda".to_string(),
                service: "test-service".to_string(),
                tags: "test:tag,env:test".to_string(),
                message: LambdaMessage {
                    message: "test-extension".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: "placeholder-request-id".to_string(),
                    },
                    timestamp: 1673061827000,
                    status: "info".to_string(),
                },
            }
        ),

        // platform init start
        platform_init_start: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::PlatformInitStart {
                    runtime_version: Some("test-runtime-version".to_string()),
                    runtime_version_arn: Some("test-runtime-version-arn".to_string()),
                    initialization_type: InitType::OnDemand,
                    phase: InitPhase::Init,
                }
            },
            IntakeLog {
                hostname: "arn".to_string(),
                source: "lambda".to_string(),
                service: "test-service".to_string(),
                tags: "test:tag,env:test".to_string(),
                message: LambdaMessage {
                    message: "INIT_START Runtime Version: test-runtime-version Runtime Version ARN: test-runtime-version-arn".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: "placeholder-request-id".to_string(),
                    },
                    timestamp: 1673061827000,
                    status: "info".to_string(),
                },
            }
        ),

        // platform start
        platform_start: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::PlatformStart {
                    request_id: "test-request-id".to_string(),
                    version: Some("test-version".to_string()),
                }
            },
            IntakeLog {
                hostname: "arn".to_string(),
                source: "lambda".to_string(),
                service: "test-service".to_string(),
                tags: "test:tag,env:test".to_string(),
                message: LambdaMessage {
                    message: "START RequestId: test-request-id Version: test-version".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: "test-request-id".to_string(),
                    },
                    timestamp: 1673061827000,
                    status: "info".to_string(),
                },
            }
        ),

        // platform runtime done
        platform_runtime_done: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::PlatformRuntimeDone {
                    request_id: "test-request-id".to_string(),
                    status: Status::Success,
                    error_type: None,
                    metrics: Some(RuntimeDoneMetrics {
                        duration_ms: 100.0,
                        produced_bytes: Some(42)
                    })
                }
            },
            IntakeLog {
                hostname: "arn".to_string(),
                source: "lambda".to_string(),
                service: "test-service".to_string(),
                tags: "test:tag,env:test".to_string(),
                message: LambdaMessage {
                    message: "END RequestId: test-request-id".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: "test-request-id".to_string(),
                    },
                    timestamp: 1673061827000,
                    status: "info".to_string(),
                },
            }
        ),

        // platform report
        platform_report: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::PlatformReport {
                    error_type: None,
                    status: Status::Success,
                    request_id: "test-request-id".to_string(),
                    metrics: ReportMetrics {
                        duration_ms: 100.0,
                        billed_duration_ms: 128,
                        memory_size_mb: 256,
                        max_memory_used_mb: 64,
                        init_duration_ms: Some(50.0),
                        restore_duration_ms: None
                    }
                }
            },
            IntakeLog {
                hostname: "arn".to_string(),
                source: "lambda".to_string(),
                service: "test-service".to_string(),
                tags: "test:tag,env:test".to_string(),
                message: LambdaMessage {
                    message: "REPORT RequestId: test-request-id Duration: 100 ms Billed Duration: 128 ms Memory Size: 256 MB Max Memory Used: 64 MB Init Duration: 50 ms".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: "test-request-id".to_string(),
                    },
                    timestamp: 1673061827000,
                    status: "info".to_string(),
                },
            }
        ),
    }
}
