use chrono::{DateTime, Utc};

use serde::Deserialize;

/// Payload received from the Telemetry API
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TelemetryEvent {
    /// Time when the telemetry was generated
    pub time: DateTime<Utc>,
    /// Telemetry record entry
    #[serde(flatten)] // TODO: Figure out if this is ideal for our use case.
    pub record: TelemetryRecord,
}

/// Record in a LambdaTelemetry entry
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", content = "record", rename_all = "lowercase")]
pub enum TelemetryRecord {
    /// Function log records
    Function(String),

    /// Extension log records
    Extension(String),

    /// Platform init start record
    #[serde(rename = "platform.initStart", rename_all = "camelCase")]
    PlatformInitStart {
        /// Type of initialization
        initialization_type: InitType,
        /// Phase of initialisation
        phase: InitPhase,
        /// Lambda runtime version
        runtime_version: Option<String>,
        /// Lambda runtime version ARN
        runtime_version_arn: Option<String>,
    },

    /// Platform init runtime done record
    #[serde(rename = "platform.initRuntimeDone", rename_all = "camelCase")]
    PlatformInitRuntimeDone {
        /// Type of initialization
        initialization_type: InitType,
        /// Phase of initialisation
        phase: Option<InitPhase>,
        /// Status of initalization
        status: Status,
        /// When the status = failure, the error_type describes what kind of error occurred
        error_type: Option<String>,
    },

    /// Platform init start record
    #[serde(rename = "platform.initReport", rename_all = "camelCase")]
    PlatformInitReport {
        /// Type of initialization
        initialization_type: InitType,
        /// Phase of initialisation
        phase: InitPhase,
        metrics: InitReportMetrics,
    },

    /// Record marking start of an invocation
    #[serde(rename = "platform.start", rename_all = "camelCase")]
    PlatformStart {
        /// Request identifier
        request_id: String,
        /// Version of the Lambda function
        version: Option<String>,
    },

    /// Record marking the completion of an invocation
    #[serde(rename = "platform.runtimeDone", rename_all = "camelCase")]
    PlatformRuntimeDone {
        /// Request identifier
        request_id: String,
        /// Status of the invocation
        status: Status,
        /// When unsuccessful, the error_type describes what kind of error occurred
        error_type: Option<String>,
        /// Metrics corresponding to the runtime
        metrics: Option<RuntimeDoneMetrics>,
    },

    /// Platfor report record
    #[serde(rename = "platform.report", rename_all = "camelCase")]
    PlatformReport {
        /// Request identifier
        request_id: String,
        /// Status of the invocation
        status: Status,
        /// When unsuccessful, the error_type describes what kind of error occurred
        error_type: Option<String>,
        metrics: ReportMetrics,
    },

    /// Extension-specific record
    #[serde(rename = "platform.extension")]
    PlatformExtension {
        /// Name of the extension
        name: String,
        /// State of the extension
        state: String,
        /// Events sent to the extension
        events: Vec<String>,
    },

    /// Telemetry processor-specific record
    #[serde(rename = "platform.telemetrySubscription")]
    PlatformTelemetrySubscription {
        /// Name of the extension
        name: String,
        /// State of the extensions
        state: String,
        /// Types of records sent to the extension
        types: Vec<String>,
    },

    /// Record generated when the telemetry processor is falling behind
    #[serde(rename = "platform.logsDropped", rename_all = "camelCase")]
    PlatformLogsDropped {
        /// Reason for dropping the logs
        reason: String,
        /// Number of records dropped
        dropped_records: u64,
        /// Total size of the dropped records
        dropped_bytes: u64,
    },
}

/// Type of Initialization
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum InitType {
    /// Initialised on demand
    OnDemand,
    /// Initialized to meet the provisioned concurrency
    ProvisionedConcurrency,
    /// SnapStart
    SnapStart,
}

/// Phase in which initialization occurs
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum InitPhase {
    /// Initialization phase
    Init,
    /// Invocation phase
    Invoke,
}

/// Status of invocation/initialization
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Success,
    Error,
    Failure,
    Timeout,
}

///Init report metrics
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitReportMetrics {
    /// Duration of initialization
    pub duration_ms: f64,
}

/// Runtime done metrics
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDoneMetrics {
    /// Duration in milliseconds
    pub duration_ms: f64,
    /// Number of bytes produced as a result of the invocation
    pub produced_bytes: Option<u64>,
}

/// Report metrics
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportMetrics {
    /// Duration in milliseconds
    pub duration_ms: f64,
    /// Billed duration in milliseconds
    pub billed_duration_ms: u64,
    /// Memory allocated in megabytes
    #[serde(rename = "memorySizeMB")]
    pub memory_size_mb: u64,
    /// Maximum memory used for the invoke in megabytes
    #[serde(rename = "maxMemoryUsedMB")]
    pub max_memory_used_mb: u64,
    /// Init duration in case of a cold start
    pub init_duration_ms: Option<f64>,
    /// Restore duration in milliseconds
    pub restore_duration_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! deserialize_tests {
        ($($name:ident: $value:expr,)*) => {
            $(
                #[test]
                fn $name() {
                    let (input, expected): (&str, TelemetryRecord) = $value;
                    let actual = serde_json::from_str::<TelemetryEvent>(&input).expect("unable to deserialize");

                    assert!(actual.record == expected);
                }
            )*
        }
    }

    deserialize_tests! {
        // function
        function: (
            r#"{"time": "2024-04-24T12:34:56.789Z","type": "function", "record": "datadog <3 serverless"}"#,
            TelemetryRecord::Function("datadog <3 serverless".to_string()),
        ),

        // extension
        extension: (
            r#"{"time": "2024-04-24T12:34:56.789Z","type": "extension", "record": "datadog <3 serverless"}"#,
            TelemetryRecord::Extension("datadog <3 serverless".to_string()),
        ),

        // platform.initStart
        platform_init_start: (
            r#"{"time":"2022-10-19T13:52:15.636Z","type":"platform.initStart","record":{"initializationType":"on-demand","phase":"init"}}"#,
            TelemetryRecord::PlatformInitStart {
                initialization_type: InitType::OnDemand,
                phase: InitPhase::Init,
                runtime_version: None,
                runtime_version_arn: None,
            },
        ),

        // platform.initRuntimeDone
        platform_init_runtime_done: (
            r#"{"time":"2022-10-19T13:52:16.136Z","type":"platform.initRuntimeDone","record":{"initializationType":"on-demand","status":"success"}}"#,
            TelemetryRecord::PlatformInitRuntimeDone {
                initialization_type: InitType::OnDemand,
                status: Status::Success,
                phase: None,
                error_type: None,
            },
        ),

        // platform.initReport
        platform_init_report: (
            r#"{"time":"2022-10-19T13:52:16.136Z","type":"platform.initReport","record":{"initializationType":"on-demand","metrics":{"durationMs":500.0},"phase":"init"}}"#,
            TelemetryRecord::PlatformInitReport {
                initialization_type: InitType::OnDemand,
                phase: InitPhase::Init,
                metrics: InitReportMetrics { duration_ms: 500.0 },
            }
        ),

        // platform.start
        platform_start: (
            r#"{"time":"2022-10-21T14:05:03.165Z","type":"platform.start","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","version":"$LATEST","tracing":{"spanId":"24cd7d670fa455f0","type":"X-Amzn-Trace-Id","value":"Root=1-6352a70e-1e2c502e358361800241fd45;Parent=35465b3a9e2f7c6a;Sampled=1"}}}"#,
            TelemetryRecord::PlatformStart {
                request_id: "459921b5-681c-4a96-beb0-81e0aa586026".to_string(),
                version: Some("$LATEST".to_string()),
            },
        ),

        // platform.runtimeDone
        platform_runtime_done: (
            r#"{"time":"2022-10-21T14:05:05.764Z","type":"platform.runtimeDone","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","status":"success","tracing":{"spanId":"24cd7d670fa455f0","type":"X-Amzn-Trace-Id","value":"Root=1-6352a70e-1e2c502e358361800241fd45;Parent=35465b3a9e2f7c6a;Sampled=1"},"spans":[{"name":"responseLatency","start":"2022-10-21T14:05:03.165Z","durationMs":2598.0},{"name":"responseDuration","start":"2022-10-21T14:05:05.763Z","durationMs":0.0}],"metrics":{"durationMs":2599.0,"producedBytes":8}}}"#,
            TelemetryRecord::PlatformRuntimeDone {
                request_id: "459921b5-681c-4a96-beb0-81e0aa586026".to_string(),
                status: Status::Success,
                error_type: None,
                metrics: Some(RuntimeDoneMetrics {
                    duration_ms: 2599.0,
                    produced_bytes: Some(8),
                }),
            },
        ),

        // platform.report
        platform_report: (
            r#"{"time":"2022-10-21T14:05:05.766Z","type":"platform.report","record":{"requestId":"459921b5-681c-4a96-beb0-81e0aa586026","metrics":{"durationMs":2599.4,"billedDurationMs":2600,"memorySizeMB":128,"maxMemoryUsedMB":94,"initDurationMs":549.04},"tracing":{"spanId":"24cd7d670fa455f0","type":"X-Amzn-Trace-Id","value":"Root=1-6352a70e-1e2c502e358361800241fd45;Parent=35465b3a9e2f7c6a;Sampled=1"},"status":"success"}}"#,
            TelemetryRecord::PlatformReport {
                request_id: "459921b5-681c-4a96-beb0-81e0aa586026".to_string(),
                status: Status::Success,
                error_type: None,
                metrics: ReportMetrics {
                    duration_ms: 2599.4,
                    billed_duration_ms: 2600,
                    memory_size_mb:128,
                    max_memory_used_mb:94,
                    init_duration_ms: Some(549.04),
                    restore_duration_ms: None,
                },
            },
        ),

         // platform.extension
         platform_extension: (
            r#"{"time":"2022-10-19T13:52:16.136Z","type":"platform.extension","record":{"name":"my-extension","state":"Ready","events":["SHUTDOWN","INVOKE"]}}"#,
            TelemetryRecord::PlatformExtension {
                name: "my-extension".to_string(),
                state: "Ready".to_string(),
                events: vec!("SHUTDOWN".to_string(), "INVOKE".to_string()),
             },
        ),

        // platform.telemetrySubscription
        platform_telemetry_subscription: (
            r#"{"time":"2022-10-19T13:52:15.667Z","type":"platform.telemetrySubscription","record":{"name":"my-extension","state":"Subscribed","types":["platform","function"]}}"#,
            TelemetryRecord::PlatformTelemetrySubscription {
                 name: "my-extension".to_string(),
                 state: "Subscribed".to_string(),
                 types: vec!("platform".to_string(), "function".to_string()),
            },
        ),

        // platform.logsDropped
        platform_logs_dropped: (
            r#"{"time":"2022-10-19T13:52:15.667Z","type":"platform.logsDropped","record":{"reason":"BufferFull","droppedRecords":1,"droppedBytes":1024}}"#,
            TelemetryRecord::PlatformLogsDropped {
                reason: "BufferFull".to_string(),
                dropped_records: 1,
                dropped_bytes: 1024,
            },
        ),
    }
}
