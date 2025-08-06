use std::error::Error;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::Sender;

use tracing::{debug, error};

use crate::LAMBDA_RUNTIME_SLUG;
use crate::config;
use crate::event_bus::Event;
use crate::lifecycle::invocation::context::Context as InvocationContext;
use crate::logs::aggregator::Aggregator;
use crate::logs::processor::{Processor, Rule};
use crate::tags::provider;
use crate::telemetry::events::{Status, TelemetryEvent, TelemetryRecord};

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
}

const OOM_ERRORS: [&str; 7] = [
    "fatal error: runtime: out of memory",       // Go
    "Java heap space: java.lang.OutOfMemoryError", // Java
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

impl Processor<IntakeLog> for LambdaProcessor {}

impl LambdaProcessor {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        datadog_config: Arc<config::Config>,
        event_bus: Sender<Event>,
    ) -> Self {
        let service = datadog_config.service.clone().unwrap_or_default();
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
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn get_message(&mut self, event: TelemetryEvent) -> Result<Message, Box<dyn Error>> {
        let copy = event.clone();
        match event.record {
            TelemetryRecord::Function(v) | TelemetryRecord::Extension(v) => {
                let message = match v {
                    serde_json::Value::Object(obj) => Some(serde_json::to_string(&obj).unwrap_or_default()),
                    serde_json::Value::String(s) => Some(s),
                    _ => None,
                };

                if let Some(message) = message {
                    if is_oom_error(&message) {
                        debug!("Lambda Processor | Got a runtime-specific OOM error. Incrementing OOM metric.");
                        if let Err(e) = self.event_bus.send(Event::OutOfMemory(event.time.timestamp())).await {
                            error!("Failed to send OOM event to the main event bus: {e}");
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
                        message.push_str(&format!(" Task timed out after {:.2} seconds", metrics.duration_ms / 1000.0));
                        result_status = "error".to_string();
                    } else if status == Status::Error {
                        message.push_str(&format!(" Task failed: {:?}", error_type.unwrap_or_default()));
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
            TelemetryRecord::PlatformReport { request_id, metrics, .. } => { // TODO: check what to do with rest of the fields
                if let Err(e) = self.event_bus.send(Event::Telemetry(copy)).await {
                    error!("Failed to send PlatformReport to the main event bus: {}", e);
                }

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

                let init_duration_ms = metrics.init_duration_ms;
                if let Some(init_duration_ms) = init_duration_ms {
                    message = format!("{message} Init Duration: {init_duration_ms:.2} ms");
                }

                Ok(Message::new(
                    message,
                    Some(request_id),
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                    None,
                ))
            },
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
                if self.invocation_context.request_id.is_empty() {
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

            IntakeLog {
                hostname: self.function_arn.clone(),
                source: LAMBDA_RUNTIME_SLUG.to_string(),
                service: self.service.clone(),
                tags,
                message: Message {
                    message: final_message,
                    lambda: lambda_message.lambda,
                    timestamp: lambda_message.timestamp,
                    status: lambda_message.status,
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

        if log.message.lambda.request_id.is_some() {
            Ok(log)
        } else {
            // No request_id available, queue as orphan log
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
        if let Some(inner_message) = json_obj.get_mut("message") {
            if let Some(serde_json::Value::String(message_tags)) = inner_message.get("ddtags") {
                tags.push(',');
                tags.push_str(message_tags);
                if let Some(inner_obj) = inner_message.as_object_mut() {
                    inner_obj.remove("ddtags");
                }
                return inner_message.to_string();
            }
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

    pub async fn process(&mut self, event: TelemetryEvent, aggregator: &Arc<Mutex<Aggregator>>) {
        if let Ok(mut log) = self.make_log(event).await {
            let should_send_log = self.logs_enabled
                && LambdaProcessor::apply_rules(&self.rules, &mut log.message.message);
            if should_send_log {
                if let Ok(serialized_log) = serde_json::to_string(&log) {
                    // explicitly drop log so we don't accidentally re-use it and push
                    // duplicate logs to the aggregator
                    drop(log);
                    self.ready_logs.push(serialized_log);
                }
            }

            // Process orphan logs, since we have a `request_id` now
            for mut orphan_log in self.orphan_logs.drain(..) {
                orphan_log.message.lambda.request_id =
                    Some(self.invocation_context.request_id.clone());
                if should_send_log {
                    if let Ok(serialized_log) = serde_json::to_string(&orphan_log) {
                        drop(orphan_log);
                        self.ready_logs.push(serialized_log);
                    }
                }
            }
        }

        if self.ready_logs.is_empty() {
            return;
        }
        let mut aggregator = aggregator.lock().expect("lock poisoned");
        aggregator.add_batch(std::mem::take(&mut self.ready_logs));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use chrono::{TimeZone, Utc};
    use serde_json::{Number, Value};
    use std::collections::hash_map::HashMap;

    use crate::logs::lambda::Lambda;
    use crate::telemetry::events::{
        InitPhase, InitType, ReportMetrics, RuntimeDoneMetrics, Status,
    };

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

        let mut processor = LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone());

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
        let mut processor = LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone());

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
        let mut processor = LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone());

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
        let mut processor = LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone());

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

    // process
    #[tokio::test]
    async fn test_process() {
        let aggregator = Arc::new(Mutex::new(Aggregator::default()));
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
            LambdaProcessor::new(Arc::clone(&tags_provider), Arc::clone(&config), tx.clone());

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor.process(event.clone(), &aggregator).await;

        let mut aggregator_lock = aggregator.lock().unwrap();
        let batch = aggregator_lock.get_batch();
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
        assert_eq!(batch, serialized_log.as_bytes());
    }

    #[tokio::test]
    async fn test_process_logs_disabled() {
        let aggregator = Arc::new(Mutex::new(Aggregator::default()));
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

        let mut processor =
            LambdaProcessor::new(Arc::clone(&tags_provider), Arc::clone(&config), tx.clone());

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor.process(event.clone(), &aggregator).await;

        let mut aggregator_lock = aggregator.lock().unwrap();
        let batch = aggregator_lock.get_batch();
        assert_eq!(batch.len(), 0);
    }

    #[tokio::test]
    async fn test_process_log_with_no_request_id() {
        let aggregator = Arc::new(Mutex::new(Aggregator::default()));
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

        let mut processor = LambdaProcessor::new(tags_provider, Arc::clone(&config), tx.clone());

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String("test-function".to_string())),
        };

        processor.process(event.clone(), &aggregator).await;
        assert_eq!(processor.orphan_logs.len(), 1);

        let mut aggregator_lock = aggregator.lock().unwrap();
        let batch = aggregator_lock.get_batch();
        assert!(batch.is_empty());
    }

    #[tokio::test]
    async fn test_process_logs_after_seeing_request_id() {
        let aggregator = Arc::new(Mutex::new(Aggregator::default()));
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
            LambdaProcessor::new(Arc::clone(&tags_provider), Arc::clone(&config), tx.clone());

        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor.process(start_event.clone(), &aggregator).await;
        assert_eq!(
            processor.invocation_context.request_id,
            "test-request-id".to_string()
        );

        // This could be any event that doesn't have a `request_id`
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function(Value::String("test-function".to_string())),
        };

        processor.process(event.clone(), &aggregator).await;

        // Verify aggregator logs
        let mut aggregator_lock = aggregator.lock().unwrap();
        let batch = aggregator_lock.get_batch();
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
        assert_eq!(batch, serialized_log.as_bytes());
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
        let mut processor =
            LambdaProcessor::new(tags_provider.clone(), Arc::clone(&config), tx.clone());
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
        let mut processor =
            LambdaProcessor::new(tags_provider.clone(), Arc::clone(&config), tx.clone());
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
}
