use std::error::Error;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tracing::debug;

use crate::config;
use crate::config::processing_rule;
use crate::logs::aggregator::Aggregator;
use crate::tags::provider;
use crate::telemetry::events::{TelemetryEvent, TelemetryRecord};
use crate::LAMBDA_RUNTIME_SLUG;

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Lambda {
    pub arn: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LambdaMessage {
    pub message: String,
    pub lambda: Lambda,
    pub timestamp: i64,
    pub status: String,
}

impl LambdaMessage {
    #[must_use]
    pub fn new(
        message: String,
        request_id: Option<String>,
        function_arn: String,
        timestamp: i64,
    ) -> LambdaMessage {
        LambdaMessage {
            message,
            lambda: Lambda {
                arn: function_arn,
                request_id,
            },
            timestamp,
            status: "info".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IntakeLog {
    pub message: LambdaMessage,
    pub hostname: String,
    pub service: String,
    #[serde(rename(serialize = "ddtags"))]
    pub tags: String,
    #[serde(rename(serialize = "ddsource"))]
    pub source: String,
}

#[derive(Clone, Debug)]
pub struct Rule {
    kind: processing_rule::Kind,
    regex: regex::Regex,
    placeholder: String,
}

#[derive(Clone)]
pub struct Processor {
    function_arn: String,
    request_id: Option<String>,
    service: String,
    tags: String,
    // Logs which don't have a `request_id`
    orphan_logs: Vec<IntakeLog>,
    // Global Processing Rules
    rules: Option<Vec<Rule>>,
}

impl Processor {
    #[must_use]
    pub fn new(
        function_arn: String,
        tags_provider: Arc<provider::Provider>,
        datadog_config: Arc<config::Config>,
    ) -> Processor {
        let service = datadog_config.service.clone().unwrap_or_default();
        let tags = tags_provider.get_tags_string();

        let processing_rules = &datadog_config.logs_config_processing_rules;
        let rules = Processor::compile_rules(processing_rules);
        Processor {
            function_arn,
            request_id: None,
            service,
            tags,
            orphan_logs: Vec::new(),
            rules,
        }
    }

    fn apply_rules(&self, log: &mut IntakeLog) -> bool {
        match &self.rules {
            // No need to apply if there are no rules
            None => true,
            Some(rules) => {
                // If rules are empty, we don't need to apply them
                if rules.is_empty() {
                    return true;
                }

                // Process rules
                for rule in rules {
                    let message = &log.message.message;
                    match rule.kind {
                        processing_rule::Kind::ExcludeAtMatch => {
                            if rule.regex.is_match(message) {
                                return false;
                            }
                        }
                        processing_rule::Kind::IncludeAtMatch => {
                            if !rule.regex.is_match(message) {
                                return false;
                            }
                        }
                        processing_rule::Kind::MaskSequences => {
                            log.message.message = rule
                                .regex
                                .replace_all(message, rule.placeholder.as_str())
                                .to_string();
                        }
                    }
                }
                true
            }
        }
    }

    fn compile_rules(
        rules: &Option<Vec<config::processing_rule::ProcessingRule>>,
    ) -> Option<Vec<Rule>> {
        match rules {
            None => None,
            Some(rules) => {
                if rules.is_empty() {
                    return None;
                }
                let mut compiled_rules = Vec::new();

                for rule in rules {
                    match regex::Regex::new(&rule.pattern) {
                        Ok(regex) => {
                            let placeholder = rule.replace_placeholder.clone().unwrap_or_default();
                            compiled_rules.push(Rule {
                                kind: rule.kind,
                                regex,
                                placeholder,
                            });
                        }
                        Err(e) => {
                            debug!("Failed to compile rule: {}", e);
                        }
                    }
                }

                Some(compiled_rules)
            }
        }
    }

    pub fn get_lambda_message(
        &mut self,
        event: TelemetryEvent,
    ) -> Result<LambdaMessage, Box<dyn Error>> {
        match event.record {
            TelemetryRecord::Function(v) | TelemetryRecord::Extension(v) => {
                Ok(LambdaMessage::new(
                    v,
                    None,
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                ))
            }
            TelemetryRecord::PlatformInitStart {
                runtime_version,
                runtime_version_arn,
                .. // TODO: check if we could do something with this metrics: `initialization_type` and `phase`
            } => {
                let rv = runtime_version.unwrap_or("?".to_string()); // TODO: check what does containers display
                let rv_arn = runtime_version_arn.unwrap_or("?".to_string()); // TODO: check what do containers display
                Ok(LambdaMessage::new(
                    format!("INIT_START Runtime Version: {rv} Runtime Version ARN: {rv_arn}"),
                    None,
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                ))
            },
            // This is the first log where `request_id` is available
            // So we set it here and use it in the unprocessed and following logs.
            TelemetryRecord::PlatformStart {
                request_id,
                version,
            } => {
                // Set request_id for unprocessed and future logs
                self.request_id = Some(request_id.clone());

                let version = version.unwrap_or("$LATEST".to_string());
                Ok(LambdaMessage::new(
                    format!("START RequestId: {request_id} Version: {version}"),
                    Some(request_id),
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                ))
            },
            TelemetryRecord::PlatformRuntimeDone { request_id , .. } => {  // TODO: check what to do with rest of the fields
                Ok(LambdaMessage::new(
                    format!("END RequestId: {request_id}"),
                    Some(request_id),
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                ))
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
                    message = format!("{message} Init Duration: {init_duration_ms} ms");
                }

                Ok(LambdaMessage::new(
                    message,
                    Some(request_id),
                    self.function_arn.clone(),
                    event.time.timestamp_millis(),
                ))
            },
            // TODO: PlatformInitRuntimeDone
            // TODO: PlatformInitReport
            // TODO: PlatformExtension
            // TODO: PlatformTelemetrySubscription
            // TODO: PlatformLogsDropped
            _ => Err("Unsupported event type".into()),
        }
    }

    pub fn get_intake_log(
        &mut self,
        mut lambda_message: LambdaMessage,
    ) -> Result<IntakeLog, Box<dyn Error>> {
        let request_id = match lambda_message.lambda.request_id {
            // Log already has a `request_id`
            Some(request_id) => Some(request_id.clone()),
            // Default to the `request_id` we've seen so far, if any.
            None => self.request_id.clone(),
        };

        lambda_message.lambda.request_id = request_id;

        let log = IntakeLog {
            hostname: self.function_arn.clone(),
            source: LAMBDA_RUNTIME_SLUG.to_string(),
            service: self.service.clone(),
            tags: self.tags.clone(),
            message: lambda_message,
        };

        if log.message.lambda.request_id.is_some() {
            Ok(log)
        } else {
            // We haven't seen a `request_id`, this is an orphan log
            self.orphan_logs.push(log);
            Err("No request_id available, queueing for later".into())
        }
    }

    pub fn make_log(&mut self, event: TelemetryEvent) -> Result<IntakeLog, Box<dyn Error>> {
        match self.get_lambda_message(event) {
            Ok(lambda_message) => self.get_intake_log(lambda_message),
            // TODO: Check what to do when we can't process the event
            Err(e) => Err(e),
        }
    }

    pub fn process(&mut self, event: TelemetryEvent, aggregator: &Arc<Mutex<Aggregator>>) {
        if let Ok(mut log) = self.make_log(event) {
            let mut aggregator = aggregator.lock().expect("lock poisoned");
            let should_send_log = self.apply_rules(&mut log);
            if should_send_log {
                aggregator.add(log);
            }

            // Process orphan logs, since we have a `request_id` now
            for mut orphan_log in self.orphan_logs.drain(..) {
                orphan_log.message.lambda.request_id = Some(
                    self.request_id
                        .clone()
                        .expect("unable to retrieve request ID"),
                );
                if should_send_log {
                    aggregator.add(orphan_log);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::logs::aggregator::Aggregator;
    use crate::tags::provider;
    use crate::telemetry::events::{
        InitPhase, InitType, ReportMetrics, RuntimeDoneMetrics, Status,
    };
    use crate::LAMBDA_RUNTIME_SLUG;
    use std::collections::hash_map::HashMap;

    use super::*;

    use chrono::{TimeZone, Utc};

    macro_rules! get_lambda_message_tests {
        ($($name:ident: $value:expr,)*) => {
            $(
                #[test]
                fn $name() {
                    let (input, expected): (&TelemetryEvent, LambdaMessage) = $value;

                    let config = Arc::new(config::Config {
                        service: Some("test-service".to_string()),
                        tags: Some("test:tags".to_string()),
                        ..config::Config::default()
                    });

                    let tags_provider = Arc::new(provider::Provider::new(Arc::clone(&config), LAMBDA_RUNTIME_SLUG.to_string(), &HashMap::new()));

                    let mut processor = Processor::new(
                        "arn".to_string(),
                        tags_provider,
                        Arc::new(config::Config {
                            service: Some("test-service".to_string()),
                            tags: Some("test:tag,env:test".to_string()),
                            ..config::Config::default()})
                    );

                    let result = processor.get_lambda_message(input.clone()).unwrap();

                    assert_eq!(result, expected);
                }
            )*
        }
    }

    // get_lambda_message
    get_lambda_message_tests! {
        // function
        function: (
            &TelemetryEvent {
                time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
                record: TelemetryRecord::Function("test-function".to_string())
            },
            LambdaMessage {
                    message: "test-function".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
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
                record: TelemetryRecord::Extension("test-extension".to_string())
            },
            LambdaMessage {
                    message: "test-extension".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
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
            LambdaMessage {
                    message: "INIT_START Runtime Version: test-runtime-version Runtime Version ARN: test-runtime-version-arn".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
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
            LambdaMessage {
                    message: "START RequestId: test-request-id Version: test-version".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
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
            LambdaMessage {
                    message: "END RequestId: test-request-id".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: Some("test-request-id".to_string()),
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
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
            LambdaMessage {
                    message: "REPORT RequestId: test-request-id Duration: 100 ms Billed Duration: 128 ms Memory Size: 256 MB Max Memory Used: 64 MB Init Duration: 50 ms".to_string(),
                    lambda: Lambda {
                        arn: "arn".to_string(),
                        request_id: Some("test-request-id".to_string()),
                    },
                    timestamp: 1_673_061_827_000,
                    status: "info".to_string(),
                },
        ),
    }

    // get_intake_log
    #[test]
    fn test_get_intake_log() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let mut processor =
            Processor::new("test-arn".to_string(), tags_provider, Arc::clone(&config));

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        let lambda_message = processor.get_lambda_message(event.clone()).unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();

        assert_eq!(intake_log.source, LAMBDA_RUNTIME_SLUG.to_string());
        assert_eq!(intake_log.hostname, "test-arn".to_string());
        assert_eq!(intake_log.service, "test-service".to_string());
        assert!(intake_log.tags.contains("test:tags"));
        assert_eq!(
            intake_log.message,
            LambdaMessage {
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

    #[test]
    fn test_get_intake_log_errors_with_orphan() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let mut processor =
            Processor::new("test-arn".to_string(), tags_provider, Arc::clone(&config));

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function("test-function".to_string()),
        };

        let lambda_message = processor.get_lambda_message(event.clone()).unwrap();
        assert_eq!(lambda_message.lambda.request_id, None);

        let intake_log = processor.get_intake_log(lambda_message).unwrap_err();
        assert_eq!(
            intake_log.to_string(),
            "No request_id available, queueing for later"
        );
        assert_eq!(processor.orphan_logs.len(), 1);
    }

    #[test]
    fn test_get_intake_log_no_orphan_after_seeing_request_id() {
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let mut processor =
            Processor::new("test-arn".to_string(), tags_provider, Arc::clone(&config));
        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        let start_lambda_message = processor.get_lambda_message(start_event.clone()).unwrap();
        processor.get_intake_log(start_lambda_message).unwrap();

        // This could be any event that doesn't have a `request_id`
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function("test-function".to_string()),
        };

        let lambda_message = processor.get_lambda_message(event.clone()).unwrap();
        let intake_log = processor.get_intake_log(lambda_message).unwrap();
        assert_eq!(
            intake_log.message.lambda.request_id,
            Some("test-request-id".to_string())
        );
    }

    // process
    #[test]
    fn test_process() {
        let aggregator = Arc::new(Mutex::new(Aggregator::default()));
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let mut processor = Processor::new(
            "test-arn".to_string(),
            Arc::clone(&tags_provider),
            Arc::clone(&config),
        );

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor.process(event.clone(), &aggregator);

        let mut aggregator_lock = aggregator.lock().unwrap();
        let batch = aggregator_lock.get_batch();
        let log = IntakeLog {
            message: LambdaMessage {
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

    #[test]
    fn test_process_log_with_no_request_id() {
        let aggregator = Arc::new(Mutex::new(Aggregator::default()));
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let mut processor =
            Processor::new("test-arn".to_string(), tags_provider, Arc::clone(&config));

        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function("test-function".to_string()),
        };

        processor.process(event.clone(), &aggregator);
        assert_eq!(processor.orphan_logs.len(), 1);

        let mut aggregator_lock = aggregator.lock().unwrap();
        let batch = aggregator_lock.get_batch();
        assert_eq!(batch, "[]".as_bytes());
    }

    #[test]
    fn test_process_logs_after_seeing_request_id() {
        let aggregator = Arc::new(Mutex::new(Aggregator::default()));
        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));
        let mut processor = Processor::new(
            "test-arn".to_string(),
            Arc::clone(&tags_provider),
            Arc::clone(&config),
        );

        let start_event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::PlatformStart {
                request_id: "test-request-id".to_string(),
                version: Some("test".to_string()),
            },
        };

        processor.process(start_event.clone(), &aggregator);
        assert_eq!(processor.request_id, Some("test-request-id".to_string()));

        // This could be any event that doesn't have a `request_id`
        let event = TelemetryEvent {
            time: Utc.with_ymd_and_hms(2023, 1, 7, 3, 23, 47).unwrap(),
            record: TelemetryRecord::Function("test-function".to_string()),
        };

        processor.process(event.clone(), &aggregator);

        // Verify aggregator logs
        let mut aggregator_lock = aggregator.lock().unwrap();
        let batch = aggregator_lock.get_batch();
        let start_log = IntakeLog {
            message: LambdaMessage {
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
            message: LambdaMessage {
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
}
