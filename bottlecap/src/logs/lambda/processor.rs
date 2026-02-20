use std::error::Error;
use std::fmt::Write;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use tracing::{debug, error};

use crate::LAMBDA_RUNTIME_SLUG;
use crate::config;
use crate::event_bus::Event;
use crate::extension::telemetry::events::ReportMetrics;
use crate::extension::telemetry::events::{Status, TelemetryEvent, TelemetryRecord};
use crate::lifecycle::invocation::context::Context as InvocationContext;
use crate::logs::aggregator_service::AggregatorHandle;
use crate::logs::processor::{Processor, Rule};
use crate::tags::provider;

use crate::logs::lambda::{IntakeLog, Message};

#[allow(clippy::module_name_repetitions)]
#[derive(Clone, Debug)]
pub struct LambdaProcessor {
    function_arn: String,
    service: String,
    tags: String,
    // Global Processing Rules
    rules: Option<Vec<Rule>>,
    // Current Invocation Context
    invocation_context: InvocationContext,
    // Logs which don't have a `request_id`
    orphan_logs: Vec<IntakeLog>,
    // Logs which are ready to be aggregated
    ready_logs: Vec<String>,
    // Main event bus
    event_bus: Sender<Event>,
    // Logs enabled
    logs_enabled: bool,
    // Managed Instance mode
    is_managed_instance_mode: bool,
}

const OOM_ERRORS: [&str; 7] = [
    "fatal error: runtime: out of memory",       // Go
    "java.lang.OutOfMemoryError",                // Java
    "JavaScript heap out of memory",             // Node
    "Runtime exited with error: signal: killed", // Node
    "MemoryError",                               // Python
    "failed to allocate memory (NoMemoryError)", // Ruby
    "OutOfMemoryException",                      // .NET
];

fn is_oom_error(error_msg: &str) -> bool {
    OOM_ERRORS
        .iter()
        .any(|&oom_str| error_msg.contains(oom_str))
}

/// Maps AWS/common log level strings to Datadog log status values.
/// Case-insensitive and accepts both short and long forms
/// (e.g. "WARN"/"WARNING", "INFO"/"INFORMATION", "ERR"/"ERROR").
/// Returns `None` for unrecognized levels so callers can fall back to a default.
fn map_log_level_to_status(level: &str) -> Option<&'static str> {
    match level.to_uppercase().as_str() {
        "FATAL" | "CRITICAL" => Some("critical"),
        "ERROR" | "ERR" => Some("error"),
        "WARN" | "WARNING" => Some("warn"),
        "INFO" | "INFORMATION" => Some("info"),
        "DEBUG" | "TRACE" => Some("debug"),
        _ => None,
    }
}

impl Processor<IntakeLog> for LambdaProcessor {}

impl LambdaProcessor {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        datadog_config: Arc<config::Config>,
        event_bus: Sender<Event>,
        is_managed_instance_mode: bool,
    ) -> Self {
        let service = datadog_config
            .service
            .clone()
            .unwrap_or_default()
            .to_lowercase();
        let tags = tags_provider.get_tags_string();
        let function_arn = tags_provider.get_canonical_id().unwrap_or_default();

        let processing_rules = &datadog_config.logs_config_processing_rules;
        let logs_enabled = datadog_config.serverless_logs_enabled;
        let rules = LambdaProcessor::compile_rules(processing_rules);
        LambdaProcessor {
            function_arn,
            service,
            tags,
            rules,
            logs_enabled,
            invocation_context: InvocationContext::default(),
            orphan_logs: Vec::new(),
            ready_logs: Vec::new(),
            event_bus,
            is_managed_instance_mode,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn get_message(&mut self, event: TelemetryEvent) -> Result<Message, Box<dyn Error>> {
        let copy = event.clone();
        match event.record {
            TelemetryRecord::Function(v) => {
                let (request_id, message) = match v {
                    serde_json::Value::Object(obj) => {
                        let request_id = if self.is_managed_instance_mode {
                            obj.get("requestId")
                                .or_else(|| obj.get("AWSRequestId"))
                                .and_then(|v| v.as_str())
                                .map(ToString::to_string)
                        } else {
                            None
                        };
                        let msg = Some(serde_json::to_string(&obj).unwrap_or_default());
                        (request_id, msg)
                    },
                    serde_json::Value::String(s) => (None, Some(s)),
                    _ => (None, None),
                };

                if let Some(message) = message {
                    if is_oom_error(&message) {
                        debug!("LOGS | Got a runtime-specific OOM error. Incrementing OOM metric.");
                        if let Err(e) = self.event_bus.send(Event::OutOfMemory(event.time.timestamp())).await {
                            error!("LOGS | Failed to send OOM event to the main event bus: {e}");
                        }
                    }

                    return Ok(Message::new(
                        message,
                        request_id,
                        self.function_arn.clone(),
                        event.time.timestamp_millis(),
                        None,
                    ));
                }

                Err("Unable to parse log".into())
            }
            TelemetryRecord::Extension(v) => {
                let message = match v {
                    serde_json::Value::Object(obj) => Some(serde_json::to_string(&obj).unwrap_or_default()),
                    serde_json::Value::String(s) => Some(s),
                    _ => None,
                };

                if let Some(message) = message {
                    if is_oom_error(&message) {
                        debug!("LOGS | Got a runtime-specific OOM error. Incrementing OOM metric.");
                        if let Err(e) = self.event_bus.send(Event::OutOfMemory(event.time.timestamp())).await {
                            error!("LOGS | Failed to send OOM event to the main event bus: {e}");
                        }
                    }

                    return Ok(Message::new(
                        message,
                        None,
                        self.function_arn.clone(),
                        event.time.timestamp_millis(),
                        None,
                    ));
                }

                Err("Unable to parse log".into())
            }
            TelemetryRecord::PlatformInitStart {
                runtime_version,
                runtime_version_arn,
                .. // TODO: check if we could do something with this metrics: `initialization_type` and `phase`
            } => {
                if let Err(e) = self.event_bus.send(Event::Telemetry(copy)).await {
                    error!("Failed to send PlatformInitStart to the main event bus: {}", e);
                }

                let rv = runtime_version.unwrap_or("?".to_string()); // TODO: check what does containers display
                let rv_arn = runtime_version_arn.unwrap_or("?".to_string()); // TODO: check what do containers display

                Ok(Message::new(
                    format!("INIT_START Runtime Version: {rv} Runtime Version ARN: {rv_arn}"),
                    None,
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                    None,
                ))
            },
            // TODO: check if we could do anything with the fields from `PlatformInitReport`
            TelemetryRecord::PlatformInitReport { .. } => {
                if let Err(e) = self.event_bus.send(Event::Telemetry(event)).await {
                    error!("Failed to send PlatformInitReport to the main event bus: {}", e);
                }
                // We don't need to process any log for this event
                Err("Unsupported event type".into())
            }
            // This is the first log where `request_id` is available
            // So we set it here and use it in the unprocessed and following logs.
            TelemetryRecord::PlatformStart {
                request_id,
                version,
            } => {
                if let Err(e) = self.event_bus.send(Event::Telemetry(copy)).await {
                    error!("Failed to send PlatformStart to the main event bus: {}", e);
                }
                // Set request_id for unprocessed and future logs
                self.invocation_context.request_id.clone_from(&request_id);

                let version = version.unwrap_or("$LATEST".to_string());
                Ok(Message::new(
                    format!("START RequestId: {request_id} Version: {version}"),
                    Some(request_id),
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                    None,
                ))
            },
            TelemetryRecord::PlatformRuntimeDone { request_id, status, metrics, error_type, .. } => {  // TODO: check what to do with rest of the fields
                if let Err(e) = self.event_bus.send(Event::Telemetry(copy)).await {
                    error!("Failed to send PlatformRuntimeDone to the main event bus: {}", e);
                }

                let mut message = format!("END RequestId: {request_id}"); 
                let mut result_status = "info".to_string();
                if let Some(metrics) = metrics {
                    self.invocation_context.runtime_duration_ms = metrics.duration_ms;
                    if status == Status::Timeout {
                        let _ = write!(message, " Task timed out after {:.2} seconds", metrics.duration_ms / 1000.0);
                        result_status = "error".to_string();
                    } else if status == Status::Error {
                        let _ = write!(message, " Task failed: {:?}", error_type.unwrap_or_default());
                        result_status = "error".to_string();
                    }
                }
                // Remove the `request_id` since no more orphan logs will be processed with this one
                self.invocation_context.request_id = String::new();

                Ok(Message::new(
                    message,
                    Some(request_id),
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                    Some(result_status),
                ))
            },
            TelemetryRecord::PlatformReport { request_id, metrics, status, error_type, .. } => {
                if let Err(e) = self.event_bus.send(Event::Telemetry(copy)).await {
                    error!("Failed to send PlatformReport to the main event bus: {}", e);
                }
                match metrics {
                    ReportMetrics::ManagedInstance(managed_instance_metrics) => {
                        let (result_status, message) = match status {
                            Status::Timeout => (
                                "error",
                                format!(
                                    "REPORT RequestId: {} Runtime Duration: {:.2} ms Task timed out after {:.2} seconds",
                                    request_id,
                                    managed_instance_metrics.duration_ms,
                                    managed_instance_metrics.duration_ms / 1000.0
                                )
                            ),
                            Status::Error => {
                                let error_detail = error_type
                                    .as_ref()
                                    .map_or_else(|| " Task failed with an unknown error".to_string(), |e| format!(" Task failed: {e}"));
                                (
                                    "error",
                                    format!(
                                        "REPORT RequestId: {} Runtime Duration: {:.2} ms{}",
                                        request_id,
                                        managed_instance_metrics.duration_ms,
                                        error_detail
                                    )
                                )
                            }
                            _ => (
                                "info",
                                format!(
                                    "REPORT RequestId: {} Runtime Duration: {:.2} ms",
                                    request_id,
                                    managed_instance_metrics.duration_ms
                                )
                            )
                        };

                        self.invocation_context.runtime_duration_ms = managed_instance_metrics.duration_ms;
                        // Remove the `request_id` since no more orphan logs will be processed with this one
                        self.invocation_context.request_id = String::new();

                        Ok(Message::new(
                            message,
                            Some(request_id),
                            self.function_arn.clone(),
                            event.time.timestamp_millis(),
                            Some(result_status.to_string()),
                        ))
                    }
                    ReportMetrics::OnDemand(metrics) => {
                        let mut post_runtime_duration_ms = 0.0;
                        // Calculate `post_runtime_duration_ms` if we've seen a `runtime_duration_ms`.
                        if self.invocation_context.runtime_duration_ms > 0.0 {
                            post_runtime_duration_ms = metrics.duration_ms - self.invocation_context.runtime_duration_ms;
                        }

                        let mut message = format!(
                            "REPORT RequestId: {} Duration: {:.2} ms Runtime Duration: {:.2} ms Post Runtime Duration: {:.2} ms Billed Duration: {:.2} ms Memory Size: {} MB Max Memory Used: {} MB",
                            request_id,
                            metrics.duration_ms,
                            self.invocation_context.runtime_duration_ms,
                            post_runtime_duration_ms,
                            metrics.billed_duration_ms,
                            metrics.memory_size_mb,
                            metrics.max_memory_used_mb,
                        );

                        if let Some(init_duration_ms) = metrics.init_duration_ms {
                            message = format!("{message} Init Duration: {init_duration_ms:.2} ms");
                        }

                        Ok(Message::new(
                            message,
                            Some(request_id),
                            self.function_arn.clone(),
                            event.time.timestamp_millis(),
                            None,
                        ))
                    }
                }
            },
            TelemetryRecord::PlatformRestoreStart { .. } => {
                if let Err(e) = self.event_bus.send(Event::Telemetry(event)).await {
                    error!("Failed to send PlatformRestoreStart to the main event bus: {}", e);
                }
                Err("Unsupported event type".into())
            }
            TelemetryRecord::PlatformRestoreReport { .. } => {
                if let Err(e) = self.event_bus.send(Event::Telemetry(event)).await {
                    error!("Failed to send PlatformRestoreReport to the main event bus: {}", e);
                }
                Err("Unsupported event type".into())
            }
            // TODO: PlatformInitRuntimeDone
            // TODO: PlatformExtension
            // TODO: PlatformTelemetrySubscription
            // TODO: PlatformLogsDropped
            _ => Err("Unsupported event type".into()),
        }
    }

    fn get_intake_log(&mut self, mut lambda_message: Message) -> Result<IntakeLog, Box<dyn Error>> {
        // Assign request_id from message or context if available
        lambda_message.lambda.request_id = match lambda_message.lambda.request_id {
            Some(request_id) => Some(request_id.clone()),
            None => {
                // If there is no request_id available in the current invocation context,
                // then set to None, same goes if we are in a Managed Instance – as concurrent
                // requests doesn't allow us to infer which invocation the logs belong to.
                if self.invocation_context.request_id.is_empty() || self.is_managed_instance_mode {
                    None
                } else {
                    Some(self.invocation_context.request_id.clone())
                }
            }
        };

        // Check if message is a JSON object that might have tags to extract
        let parsed_json =
            serde_json::from_str::<serde_json::Value>(lambda_message.message.as_str());

        let log = if let Ok(serde_json::Value::Object(mut json_obj)) = parsed_json {
            let mut tags = self.tags.clone();
            let final_message = Self::extract_tags_and_get_message(
                &mut json_obj,
                &mut tags,
                lambda_message.message.clone(),
            );

            // Extract log level from JSON (AWS JSON log format / Powertools)
            let status = json_obj
                .get("level")
                .and_then(|v| v.as_str())
                .and_then(map_log_level_to_status)
                .map_or(lambda_message.status.clone(), std::string::ToString::to_string);

            IntakeLog {
                hostname: self.function_arn.clone(),
                source: LAMBDA_RUNTIME_SLUG.to_string(),
                service: self.service.clone(),
                tags,
                message: Message {
                    message: final_message,
                    lambda: lambda_message.lambda,
                    timestamp: lambda_message.timestamp,
                    status,
                },
            }
        } else {
            // Not JSON or not an object - use message as-is
            IntakeLog {
                hostname: self.function_arn.clone(),
                source: LAMBDA_RUNTIME_SLUG.to_string(),
                service: self.service.clone(),
                tags: self.tags.clone(),
                message: lambda_message,
            }
        };

        if log.message.lambda.request_id.is_some() || self.is_managed_instance_mode {
            // In On-Demand mode, ship logs with request_id.
            // In Managed Instance mode, ship logs without request_id immediately as well.
            // These are inter-invocation/sandbox logs that should be aggregated without
            // waiting to be attached to the next invocation.
            Ok(log)
        } else {
            // In On-Demand mode, if no request_id is available, queue as orphan log
            self.orphan_logs.push(log);
            Err("No request_id available, queueing for later".into())
        }
    }

    fn extract_tags_and_get_message(
        json_obj: &mut serde_json::Map<String, serde_json::Value>,
        tags: &mut String,
        original_message: String,
    ) -> String {
        // Check for top-level ddtags
        if let Some(serde_json::Value::String(message_tags)) = json_obj.get("ddtags") {
            tags.push(',');
            tags.push_str(message_tags);
            json_obj.remove("ddtags");
            return serde_json::to_string(json_obj).unwrap_or(original_message);
        }

        // Check for nested ddtags inside a "message" field
        if let Some(inner_message) = json_obj.get_mut("message")
            && let Some(serde_json::Value::String(message_tags)) = inner_message.get("ddtags")
        {
            tags.push(',');
            tags.push_str(message_tags);
            if let Some(inner_obj) = inner_message.as_object_mut() {
                inner_obj.remove("ddtags");
            }
            return inner_message.to_string();
        }

        // No ddtags found, use original message
        original_message
    }

    async fn make_log(&mut self, event: TelemetryEvent) -> Result<IntakeLog, Box<dyn Error>> {
        match self.get_message(event).await {
            Ok(lambda_message) => self.get_intake_log(lambda_message),
            // TODO: Check what to do when we can't process the event
            Err(e) => Err(e),
        }
    }

    /// Processes a log, applies filtering rules, serializes it, and queues it for aggregation
    fn process_and_queue_log(&mut self, mut log: IntakeLog) {
        let should_send_log = self.logs_enabled
            && LambdaProcessor::apply_rules(&self.rules, &mut log.message.message);
        if should_send_log && let Ok(serialized_log) = serde_json::to_string(&log) {
            // explicitly drop log so we don't accidentally re-use it and push
            // duplicate logs to the aggregator
            drop(log);
            self.ready_logs.push(serialized_log);
        }
    }

    pub async fn process(&mut self, event: TelemetryEvent, aggregator_handle: &AggregatorHandle) {
        if let Ok(log) = self.make_log(event).await {
            self.process_and_queue_log(log);

            // Process orphan logs, since we have a `request_id` now
            let orphan_logs = std::mem::take(&mut self.orphan_logs);
            for mut orphan_log in orphan_logs {
                orphan_log.message.lambda.request_id =
                    Some(self.invocation_context.request_id.clone());
                self.process_and_queue_log(orphan_log);
            }
        }

        if !self.ready_logs.is_empty()
            && let Err(e) = aggregator_handle.insert_batch(std::mem::take(&mut self.ready_logs))
        {
            debug!("Failed to send logs to aggregator: {}", e);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use chrono::{TimeZone, Utc};
    use serde_json::{Number, Value};
    use std::collections::hash_map::HashMap;
    use std::sync::Arc;

    use crate::extension::telemetry::events::{
        InitPhase, InitType, ManagedInstanceReportMetrics, OnDemandReportMetrics, ReportMetrics,
        RuntimeDoneMetrics, Status,
    };
    use crate::logs::aggregator_service::AggregatorService;
    use crate::logs::lambda::Lambda;

    macro_rules! get_message_tests {
        ($($name:ident: $value:expr,)*) => {
            $(
                #[tokio::test]
                async fn $name() {
                    let (input, expected): (&TelemetryEvent, Message) = $value;

                    let tags = HashMap::from([("test".to_string(), "tags".to_string())]);
                    let config = Arc::new(config::Config {
                        service: Some("test-service".to_string()),
                        tags: tags.clone(),
                        ..config::Config::default()
                    });

                    let tags_provider = Arc::new(
                        provider::Provider::new(Arc::clone(&config),
                        LAMBDA_RUNTIME_SLUG.to_string(),
                        &HashMap::from([("function_arn".to_string(), "test-arn".to_string())])));

                    let (tx, _) = tokio::sync::mpsc::channel(2);

                    let mut processor = LambdaProcessor::new(
                        tags_provider,
                        Arc::new(config::Config {
                            service: Some("test-service".to_string()),
                            tags,
                            ..config::Config::default()}),
                        tx.clone(),
                        false, // On-Demand mode
                    );

                    let result = processor.get_message(input.clone()).await.unwrap();

                    assert_eq!(result, expected);
                }
            )*
        }
    }

    // get_message
    get_message_tests! {
        // function
        function: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::Function(Value::String("test-function".to_string()))
            },
            Message {
                    message: "test-function".to_string(),
                    lambda: Lambda {
                        arn: "test-arn".to_string(),
                        request_id: None,
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
                },
        ),

        // extension
        extension: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::Extension(Value::String("test-extension".to_string()))
            },
            Message {
                    message: "test-extension".to_string(),
                    lambda: Lambda {
                        arn: "test-arn".to_string(),
                        request_id: None,
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
                },
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
            Message {
                    message: "INIT_START Runtime Version: test-runtime-version Runtime Version ARN: test-runtime-version-arn".to_string(),
                    lambda: Lambda {
                        arn: "test-arn".to_string(),
                        request_id: None,
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
                },
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
            Message {
                    message: "START RequestId: test-request-id Version: test-version".to_string(),
                    lambda: Lambda {
                        arn: "test-arn".to_string(),
                        request_id: Some("test-request-id".to_string()),
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
                },
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
            Message {
                    message: "END RequestId: test-request-id".to_string(),
                    lambda: Lambda {
                        arn: "test-arn".to_string(),
                        request_id: Some("test-request-id".to_string()),
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
                },
        ),

        // platform runtime done
        platform_runtime_done_timeout: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::PlatformRuntimeDone {
                    request_id: "test-request-id".to_string(),
                    status: Status::Timeout,
                    error_type: None,
                    metrics: Some(RuntimeDoneMetrics {
                        duration_ms: 5000.0,
                        produced_bytes: Some(42)
                    })
                }
            },
            Message {
                    message: "END RequestId: test-request-id Task timed out after 5.00 seconds".to_string(),
                    lambda: Lambda {
                        arn: "test-arn".to_string(),
                        request_id: Some("test-request-id".to_string()),
                    },
                    timestamp: 1_673_061_827_000,
                    status: "error".to_string(),
                },
        ),

        // platform report
        platform_report: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::PlatformReport {
                    error_type: None,
                    status: Status::Success,
                    request_id: "test-request-id".to_string(),
                    metrics: ReportMetrics::OnDemand(OnDemandReportMetrics {
                        duration_ms: 100.0,
                        billed_duration_ms: 128,
                        memory_size_mb: 256,
                        max_memory_used_mb: 64,
                        init_duration_ms: Some(50.0),
                        restore_duration_ms: None
                    }),
                    spans: None,
                }
            },
            Message {
                    message: "REPORT RequestId: test-request-id Duration: 100.00 ms Runtime Duration: 0.00 ms Post Runtime Duration: 0.00 ms Billed Duration: 128 ms Memory Size: 256 MB Max Memory Used: 64 MB Init Duration: 50.00 ms".to_string(),
                    lambda: Lambda {
                        arn: "test-arn".to_string(),
                        request_id: Some("test-request-id".to_string()),
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
                },
        ),

        // platform report - Managed Instance mode success
        platform_report_managed_instance_success: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 2, 30, 27).unwrap(),
                record: TelemetryRecord::PlatformReport {
                    request_id: "test-request-id".to_string(),
                    metrics: ReportMetrics::ManagedInstance(ManagedInstanceReportMetrics {
                        duration_ms: 123.45,
                    }),
                    status: Status::Success,
                    error_type: None,
                    spans: None,
                }
            },
            Message {
                message: "REPORT RequestId: test-request-id Runtime Duration: 123.45 ms".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_058_627_000,
                status: "info".to_string(),
            },
        ),

        // platform report - managed instance mode error with error_type
        platform_report_managed_instance_error: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 2, 30, 27).unwrap(),
                record: TelemetryRecord::PlatformReport {
                    request_id: "test-request-id".to_string(),
                    metrics: ReportMetrics::ManagedInstance(ManagedInstanceReportMetrics {
                        duration_ms: 200.0,
                    }),
                    status: Status::Error,
                    error_type: Some("RuntimeError".to_string()),
                    spans: None,
                }
            },
            Message {
                message: "REPORT RequestId: test-request-id Runtime Duration: 200.00 ms Task failed: RuntimeError".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_058_627_000,
                status: "error".to_string(),
            },
        ),

        // platform report - managed instance mode timeout
        platform_report_managed_instance_timeout: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 2, 30, 27).unwrap(),
                record: TelemetryRecord::PlatformReport {
                    request_id: "test-request-id".to_string(),
                    metrics: ReportMetrics::ManagedInstance(ManagedInstanceReportMetrics {
                        duration_ms: 30000.0,
                    }),
                    status: Status::Timeout,
                    error_type: None,
                    spans: None,
                }
            },
            Message {
                message: "REPORT RequestId: test-request-id Runtime Duration: 30000.00 ms Task timed out after 30.00 seconds".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_058_627_000,
                status: "error".to_string(),
            },
        ),

        // platform report - managed instance mode error without error_type
        platform_report_managed_instance_error_no_type: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 2, 30, 27).unwrap(),
                record: TelemetryRecord::PlatformReport {
                    request_id: "test-request-id".to_string(),
                    metrics: ReportMetrics::ManagedInstance(ManagedInstanceReportMetrics {
                        duration_ms: 150.0,
                    }),
                    status: Status::Error,
                    error_type: None,
                    spans: None,
                }
            },
            Message {
                message: "REPORT RequestId: test-request-id Runtime Duration: 150.00 ms Task failed with an unknown error".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_058_627_000,
                status: "error".to_string(),
            },
        ),
    }

    #[tokio::test]
    async fn test_get_message_function_unsupported_value() {
        let config = Arc::new(config::Config {
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _) = tokio::sync::mpsc::channel(2);

        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::Number(Number::from(12))),
        };

        let result = processor.get_message(event).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unable to parse log")
        );
    }

    // get_intake_log
    #[tokio::test]
    async fn test_get_intake_log() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        let lambda_message = processor.get_message(event.clone()).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();

        assert_eq!(intake_log.source, LAMBDA_RUNTIME_SLUG.to_string());
        assert_eq!(intake_log.hostname, "test-arn".to_string());
        assert_eq!(intake_log.service, "test-service".to_string());
        assert!(intake_log.tags.contains("test:tags"));
        assert_eq!(
            intake_log.message,
            Message {
                message: "START RequestId: test-request-id Version: test".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_061_827_000,
                status: "info".to_string(),
            },
        );
    }

    #[tokio::test]
    async fn test_get_intake_log_errors_with_orphan() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String("test-function".to_string())),
        };

        let lambda_message = processor.get_message(event.clone()).await.unwrap();
        assert_eq!(lambda_message.lambda.request_id, None);

        let intake_log = processor.get_intake_log(lambda_message).unwrap_err();
        assert_eq!(
            intake_log.to_string(),
            "No request_id available, queueing for later"
        );
        assert_eq!(processor.orphan_logs.len(), 1);
    }

    #[tokio::test]
    async fn test_get_intake_log_no_orphan_after_seeing_request_id() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        let start_lambda_message = processor.get_message(start_event.clone()).await.unwrap();
        processor.get_intake_log(start_lambda_message).unwrap();

        // This could be any event that doesn't have a `request_id`
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String("test-function".to_string())),
        };

        let lambda_message = processor.get_message(event.clone()).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(
            intake_log.message.lambda.request_id,
            Some("test-request-id".to_string())
        );
    }

    #[tokio::test]
    async fn test_get_intake_log_managed_instance_mode() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);

        // Set is_managed_instance_mode to true
        let mut processor =
            LambdaProcessor::new(tags_provider.clone(), Arc::clone(&config), tx.clone(), true);

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String("test-function".to_string())),
        };

        let lambda_message = processor.get_message(event.clone()).await.unwrap();
        assert_eq!(lambda_message.lambda.request_id, None);

        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.lambda.request_id, None);
        assert_eq!(processor.orphan_logs.len(), 0);

        assert_eq!(intake_log.source, LAMBDA_RUNTIME_SLUG.to_string());
        assert_eq!(intake_log.hostname, "test-arn".to_string());
        assert_eq!(intake_log.service, "test-service".to_string());
        assert_eq!(intake_log.message.message, "test-function".to_string());
        assert_eq!(intake_log.tags, tags_provider.get_tags_string());
    }

    // process
    #[tokio::test]
    async fn test_process() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let (aggregator_service, aggregator_handle) = AggregatorService::default();

        let service_handle = tokio::spawn(async move {
            aggregator_service.run().await;
        });

        let mut processor = LambdaProcessor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            tx.clone(),
            false,
        );

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor.process(event.clone(), &aggregator_handle).await;

        let batches = aggregator_handle.get_batches().await.unwrap();
        assert_eq!(batches.len(), 1);

        let log = IntakeLog {
            message: Message {
                message: "START RequestId: test-request-id Version: test".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_061_827_000,
                status: "info".to_string(),
            },
            hostname: "test-arn".to_string(),
            source: LAMBDA_RUNTIME_SLUG.to_string(),
            service: "test-service".to_string(),
            tags: tags_provider.get_tags_string(),
        };
        let serialized_log = format!("[{}]", serde_json::to_string(&log).unwrap());
        assert_eq!(batches[0], serialized_log.as_bytes());

        aggregator_handle
            .shutdown()
            .expect("Failed to shutdown aggregator service");
        service_handle
            .await
            .expect("Aggregator service task failed");
    }

    #[tokio::test]
    async fn test_process_logs_disabled() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            serverless_logs_enabled: false,
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);

        let (aggregator_service, aggregator_handle) = AggregatorService::default();
        let service_handle = tokio::spawn(async move {
            aggregator_service.run().await;
        });

        let mut processor = LambdaProcessor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            tx.clone(),
            false,
        );

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor.process(event.clone(), &aggregator_handle).await;

        let batches = aggregator_handle.get_batches().await.unwrap();
        assert!(batches.is_empty());

        aggregator_handle
            .shutdown()
            .expect("Failed to shutdown aggregator service");
        service_handle
            .await
            .expect("Aggregator service task failed");
    }

    #[tokio::test]
    async fn test_process_log_with_no_request_id() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);

        let (aggregator_service, aggregator_handle) = AggregatorService::default();
        let service_handle = tokio::spawn(async move {
            aggregator_service.run().await;
        });

        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String("test-function".to_string())),
        };

        processor.process(event.clone(), &aggregator_handle).await;
        assert_eq!(processor.orphan_logs.len(), 1);

        let batches = aggregator_handle.get_batches().await.unwrap();
        assert!(batches.is_empty());

        aggregator_handle
            .shutdown()
            .expect("Failed to shutdown aggregator service");
        service_handle
            .await
            .expect("Aggregator service task failed");
    }

    #[tokio::test]
    async fn test_process_logs_after_seeing_request_id() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);

        let (aggregator_service, aggregator_handle) = AggregatorService::default();
        let service_handle = tokio::spawn(async move {
            aggregator_service.run().await;
        });

        let mut processor = LambdaProcessor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            tx.clone(),
            false,
        );

        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor
            .process(start_event.clone(), &aggregator_handle)
            .await;
        assert_eq!(
            processor.invocation_context.request_id,
            "test-request-id".to_string()
        );

        // This could be any event that doesn't have a `request_id`
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String("test-function".to_string())),
        };

        processor.process(event.clone(), &aggregator_handle).await;

        let batches = aggregator_handle.get_batches().await.unwrap();
        assert_eq!(batches.len(), 1);

        let start_log = IntakeLog {
            message: Message {
                message: "START RequestId: test-request-id Version: test".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_061_827_000,
                status: "info".to_string(),
            },
            hostname: "test-arn".to_string(),
            source: LAMBDA_RUNTIME_SLUG.to_string(),
            service: "test-service".to_string(),
            tags: tags_provider.get_tags_string(),
        };
        let function_log = IntakeLog {
            message: Message {
                message: "test-function".to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_061_827_000,
                status: "info".to_string(),
            },
            hostname: "test-arn".to_string(),
            source: LAMBDA_RUNTIME_SLUG.to_string(),
            service: "test-service".to_string(),
            tags: tags_provider.get_tags_string(),
        };
        let serialized_log = format!(
            "[{},{}]",
            serde_json::to_string(&start_log).unwrap(),
            serde_json::to_string(&function_log).unwrap()
        );
        assert_eq!(batches[0], serialized_log.as_bytes());

        aggregator_handle
            .shutdown()
            .expect("Failed to shutdown aggregator service");
        service_handle
            .await
            .expect("Aggregator service task failed");
    }

    #[tokio::test]
    async fn test_process_logs_structured_ddtags() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);

        let mut processor = LambdaProcessor::new(
            tags_provider.clone(),
            Arc::clone(&config),
            tx.clone(),
            false,
        );
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        let start_lambda_message = processor.get_message(start_event.clone()).await.unwrap();
        processor.get_intake_log(start_lambda_message).unwrap();
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String(r#"{"message":{"custom_details": "my-structured-message","ddtags":"added_tag1:added_value1,added_tag2:added_value2"}}"#.to_string())),
        };

        let lambda_message = processor.get_message(event.clone()).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();

        assert_eq!(intake_log.source, LAMBDA_RUNTIME_SLUG.to_string());
        assert_eq!(intake_log.hostname, "test-arn".to_string());
        assert_eq!(intake_log.service, "test-service".to_string());
        assert!(intake_log.tags.contains("added_tag1:added_value1"));
        let function_log = IntakeLog {
            message: Message {
                message: r#"{"custom_details":"my-structured-message"}"#.to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_061_827_000,
                status: "info".to_string(),
            },
            hostname: "test-arn".to_string(),
            source: LAMBDA_RUNTIME_SLUG.to_string(),
            service: "test-service".to_string(),
            tags: tags_provider.get_tags_string()
                + ",added_tag1:added_value1,added_tag2:added_value2",
        };
        assert_eq!(intake_log, function_log);
    }
    #[tokio::test]
    async fn test_process_logs_structured_no_ddtags() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);

        let mut processor = LambdaProcessor::new(
            tags_provider.clone(),
            Arc::clone(&config),
            tx.clone(),
            false,
        );
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        let start_lambda_message = processor.get_message(start_event.clone()).await.unwrap();
        processor.get_intake_log(start_lambda_message).unwrap();
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String(r#"{"message":{"custom_details":"my-structured-message"},"my_other_details":"included"}"#.to_string())),
        };

        let lambda_message = processor.get_message(event.clone()).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();

        assert_eq!(intake_log.source, LAMBDA_RUNTIME_SLUG.to_string());
        assert_eq!(intake_log.hostname, "test-arn".to_string());
        assert_eq!(intake_log.service, "test-service".to_string());
        let function_log = IntakeLog {
            message: Message {
                message: r#"{"message":{"custom_details":"my-structured-message"},"my_other_details":"included"}"#.to_string(),
                lambda: Lambda {
                    arn: "test-arn".to_string(),
                    request_id: Some("test-request-id".to_string()),
                },
                timestamp: 1_673_061_827_000,
                status: "info".to_string(),
            },
            hostname: "test-arn".to_string(),
            source: LAMBDA_RUNTIME_SLUG.to_string(),
            service: "test-service".to_string(),
            tags: tags_provider.get_tags_string(),
        };
        assert_eq!(intake_log, function_log);
    }

    #[tokio::test]
    async fn test_process_orphan_logs_with_exclude_at_match_rule() {
        use crate::config::processing_rule;

        // This test verifies that the orphan logs are not excluded when the START/END/REPORT
        // logs matched an exclude_at_match rule.
        let processing_rules = vec![processing_rule::ProcessingRule {
            kind: processing_rule::Kind::ExcludeAtMatch,
            name: "exclude_start_and_end_logs".to_string(),
            pattern: "(START|END|REPORT) RequestId".to_string(),
            replace_placeholder: None,
        }];

        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            logs_config_processing_rules: Some(processing_rules),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let (aggregator_service, aggregator_handle) = AggregatorService::default();
        let service_handle = tokio::spawn(async move {
            aggregator_service.run().await;
        });

        let mut processor = LambdaProcessor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            tx.clone(),
            false,
        );

        // First, send an extension log (orphan) that doesn't have a request_id
        let extension_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Extension(Value::String(
                "[Extension] Important extension log that should not be excluded".to_string(),
            )),
        };
        processor
            .process(extension_event.clone(), &aggregator_handle)
            .await;

        // Verify the extension log is queued as an orphan
        assert_eq!(processor.orphan_logs.len(), 1);

        // Send a user error log (orphan) that also doesn't have a request_id
        let error_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 48).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                "Error: NO_REGION environment variable not set".to_string(),
            )),
        };
        processor
            .process(error_event.clone(), &aggregator_handle)
            .await;

        // Now we should have 2 orphan logs
        assert_eq!(processor.orphan_logs.len(), 2);

        // Now send a PlatformStart event that should be excluded by the rule
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 49).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };
        processor
            .process(start_event.clone(), &aggregator_handle)
            .await;

        // The START log should be excluded, but the orphan logs should be sent
        // because they don't match the exclude pattern
        let batches = aggregator_handle.get_batches().await.unwrap();
        assert_eq!(batches.len(), 1);

        let batch_str = String::from_utf8(batches[0].clone()).unwrap();

        // Verify the START log is NOT in the batch (it should be excluded)
        assert!(!batch_str.contains("START RequestId"));

        // Verify the extension log IS in the batch
        assert!(batch_str.contains("Important extension log that should not be excluded"));

        // Verify the error log IS in the batch
        assert!(batch_str.contains("NO_REGION environment variable not set"));

        // Verify both logs have the correct request_id assigned
        assert!(batch_str.contains("test-request-id"));

        aggregator_handle
            .shutdown()
            .expect("Failed to shutdown aggregator service");
        service_handle
            .await
            .expect("Aggregator service task failed");
    }

    #[tokio::test]
    async fn test_orphan_logs_no_request_id_in_managed_instance() {
        // This test verifies that in managed instance mode, logs without a request_id
        // are sent immediately without being queued as orphans, and they never get
        // a request_id attached even when one becomes available in the invocation context.
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);

        // Create processor in managed instance mode (is_managed_instance_mode = true)
        let mut processor = LambdaProcessor::new(
            Arc::clone(&tags_provider),
            Arc::clone(&config),
            tx.clone(),
            true, // Managed Instance mode
        );

        // Send a function log without a request_id (inter-invocation log)
        let function_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                "Inter-invocation log without request_id".to_string(),
            )),
        };
        let log1 = processor.make_log(function_event).await.unwrap();
        assert_eq!(log1.message.lambda.request_id, None);
        assert_eq!(processor.orphan_logs.len(), 0);

        // Now send a PlatformStart event with a request_id to set the context
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 49).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };
        processor.make_log(start_event).await.unwrap();
        assert_eq!(processor.invocation_context.request_id, "test-request-id");

        // Send another function log without explicit request_id
        let another_function_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 50).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                "Another log after request_id available".to_string(),
            )),
        };
        let log2 = processor.make_log(another_function_event).await.unwrap();

        // In managed instance mode, logs should not inherit request_id from context
        assert_eq!(log2.message.lambda.request_id, None);
        assert_eq!(processor.orphan_logs.len(), 0);
    }

    #[tokio::test]
    async fn test_lmi_extracts_request_id_from_function_json() {
        let tags = HashMap::from([("test".to_string(), "tags".to_string())]);
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: tags.clone(),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _) = tokio::sync::mpsc::channel(2);

        let mut processor = LambdaProcessor::new(
            tags_provider,
            Arc::new(config::Config {
                service: Some("test-service".to_string()),
                tags,
                ..config::Config::default()
            }),
            tx.clone(),
            true, // LMI mode
        );

        // Test with "requestId" field
        let mut obj = serde_json::Map::new();
        obj.insert(
            "requestId".to_string(),
            Value::String("test-request-123".to_string()),
        );
        obj.insert(
            "message".to_string(),
            Value::String("Hello World".to_string()),
        );

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::Object(obj)),
        };

        let result = processor.get_message(event).await.unwrap();
        assert_eq!(
            result.lambda.request_id,
            Some("test-request-123".to_string())
        );
    }

    #[tokio::test]
    async fn test_regular_lambda_does_not_extract_request_id() {
        let tags = HashMap::from([("test".to_string(), "tags".to_string())]);
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: tags.clone(),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _) = tokio::sync::mpsc::channel(2);

        let mut processor = LambdaProcessor::new(
            tags_provider,
            Arc::new(config::Config {
                service: Some("test-service".to_string()),
                tags,
                ..config::Config::default()
            }),
            tx.clone(),
            false, // Regular Lambda mode (not LMI)
        );

        // Test that requestId is NOT extracted in regular Lambda mode
        let mut obj = serde_json::Map::new();
        obj.insert(
            "requestId".to_string(),
            Value::String("test-request-789".to_string()),
        );
        obj.insert(
            "message".to_string(),
            Value::String("Hello World".to_string()),
        );

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::Object(obj)),
        };

        let result = processor.get_message(event).await.unwrap();
        // Should be None because we're not in LMI mode
        assert_eq!(result.lambda.request_id, None);
    }

    #[test]
    fn test_map_log_level_to_status() {
        // AWS JSON log format levels (uppercase)
        assert_eq!(map_log_level_to_status("WARN"), Some("warn"));
        assert_eq!(map_log_level_to_status("ERROR"), Some("error"));
        assert_eq!(map_log_level_to_status("INFO"), Some("info"));
        assert_eq!(map_log_level_to_status("DEBUG"), Some("debug"));
        assert_eq!(map_log_level_to_status("FATAL"), Some("critical"));
        assert_eq!(map_log_level_to_status("TRACE"), Some("debug"));

        // Case-insensitive (lowercase, mixed case, PascalCase)
        assert_eq!(map_log_level_to_status("warn"), Some("warn"));
        assert_eq!(map_log_level_to_status("error"), Some("error"));
        assert_eq!(map_log_level_to_status("Warn"), Some("warn"));
        assert_eq!(map_log_level_to_status("Info"), Some("info"));
        assert_eq!(map_log_level_to_status("debug"), Some("debug"));
        assert_eq!(map_log_level_to_status("Fatal"), Some("critical"));
        assert_eq!(map_log_level_to_status("trace"), Some("debug"));

        // Short-form aliases
        assert_eq!(map_log_level_to_status("ERR"), Some("error"));
        assert_eq!(map_log_level_to_status("err"), Some("error"));

        // Long-form variants (.NET LogLevel names, syslog, etc.)
        assert_eq!(map_log_level_to_status("WARNING"), Some("warn"));
        assert_eq!(map_log_level_to_status("Warning"), Some("warn"));
        assert_eq!(map_log_level_to_status("warning"), Some("warn"));
        assert_eq!(map_log_level_to_status("INFORMATION"), Some("info"));
        assert_eq!(map_log_level_to_status("Information"), Some("info"));
        assert_eq!(map_log_level_to_status("information"), Some("info"));
        assert_eq!(map_log_level_to_status("CRITICAL"), Some("critical"));
        assert_eq!(map_log_level_to_status("Critical"), Some("critical"));

        // Unrecognized levels
        assert_eq!(map_log_level_to_status("UNKNOWN"), None);
        assert_eq!(map_log_level_to_status("VERBOSE"), None);
        assert_eq!(map_log_level_to_status(""), None);
    }

    #[tokio::test]
    async fn test_get_intake_log_extracts_level_from_json() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        // Set request_id so logs are not orphaned
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };
        let start_msg = processor.get_message(start_event).await.unwrap();
        processor.get_intake_log(start_msg).unwrap();

        // Test WARN level
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 48).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                r#"{"timestamp":"2025-08-27T10:25:22.244Z","level":"WARN","message":"This is a warning"}"#.to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "warn");

        // Test ERROR level
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 49).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                r#"{"timestamp":"2025-08-27T10:25:22.244Z","level":"ERROR","message":"This is an error"}"#.to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "error");

        // Test FATAL level
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 50).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                r#"{"timestamp":"2025-08-27T10:25:22.244Z","level":"FATAL","message":"Fatal error"}"#.to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "critical");

        // Test DEBUG level
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 51).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                r#"{"timestamp":"2025-08-27T10:25:22.244Z","level":"DEBUG","message":"Debug info"}"#.to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "debug");

        // Test INFO level (should remain "info")
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 52).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                r#"{"timestamp":"2025-08-27T10:25:22.244Z","level":"INFO","message":"Info message"}"#.to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "info");
    }

    #[tokio::test]
    async fn test_get_intake_log_no_level_defaults_to_info() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        // Set request_id
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };
        let start_msg = processor.get_message(start_event).await.unwrap();
        processor.get_intake_log(start_msg).unwrap();

        // JSON without level field
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 48).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                r#"{"message":"No level field here"}"#.to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "info");
    }

    #[tokio::test]
    async fn test_get_intake_log_unrecognized_level_defaults_to_info() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        // Set request_id
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };
        let start_msg = processor.get_message(start_event).await.unwrap();
        processor.get_intake_log(start_msg).unwrap();

        // JSON with unrecognized level
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 48).unwrap(),
            record: TelemetryRecord::Function(Value::String(
                r#"{"level":"VERBOSE","message":"Unknown level"}"#.to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "info");
    }

    #[tokio::test]
    async fn test_platform_event_status_not_overridden_by_level() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), false);

        // PlatformRuntimeDone with timeout should keep "error" status
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformRuntimeDone {
                request_id: "test-request-id".to_string(),
                status: Status::Timeout,
                error_type: None,
                metrics: Some(RuntimeDoneMetrics {
                    duration_ms: 5000.0,
                    produced_bytes: Some(42),
                }),
            },
        };

        let lambda_message = processor.get_message(event).await.unwrap();
        assert_eq!(lambda_message.status, "error");

        // The intake log should preserve the "error" status (message is not JSON)
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "error");
    }

    #[tokio::test]
    async fn test_extension_json_log_extracts_level() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let mut processor =
            LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone(), true);

        // Extension log from our JSON formatter: {"level":"ERROR","message":"DD_EXTENSION | ERROR | ..."}
        // Arrives as a string since it was written to stderr as a JSON line
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Extension(Value::String(
                r#"{"level":"ERROR","message":"DD_EXTENSION | ERROR | Extension loop failed"}"#
                    .to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "error");
        assert!(
            intake_log
                .message
                .message
                .contains("DD_EXTENSION | ERROR |")
        );

        // DEBUG level
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 48).unwrap(),
            record: TelemetryRecord::Extension(Value::String(
                r#"{"level":"DEBUG","message":"DD_EXTENSION | DEBUG | Starting extension"}"#
                    .to_string(),
            )),
        };
        let lambda_message = processor.get_message(event).await.unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(intake_log.message.status, "debug");
    }
}
