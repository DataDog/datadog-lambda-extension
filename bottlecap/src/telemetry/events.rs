use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};

/// Payload received from the Telemetry API
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TelemetryEvent {
    /// Time when the telemetry was generated
    pub time: DateTime<Utc>,
    /// Telemetry record entry
    #[serde(flatten)] // TODO: Figure out if this is ideal for our use case.
    pub record: TelemetryRecord,
}

/// Record in a LambdaTelemetry entry
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        runtime_version: Option<String>,
        /// Lambda runtime version ARN
        #[serde(skip_serializing_if = "Option::is_none")]
        runtime_version_arn: Option<String>,
    },

    /// Platform init runtime done record
    #[serde(rename = "platform.initRuntimeDone", rename_all = "camelCase")]
    PlatformInitRuntimeDone {
        /// Type of initialization
        initialization_type: InitType,
        /// Phase of initialisation
        #[serde(skip_serializing_if = "Option::is_none")]
        phase: Option<InitPhase>,
        /// Status of initalization
        status: Status,
        /// When the status = failure, the error_type describes what kind of error occurred
        #[serde(skip_serializing_if = "Option::is_none")]
        error_type: Option<String>,
    },

    /// Platform init start record
    #[serde(rename = "platform.initReport", rename_all = "camelCase")]
    PlatformInitReport {
        /// Type of initialization
        initialization_type: InitType,
        /// Phase of initialisation
        phase: InitPhase,
        /// Metrics
        metrics: InitReportMetrics,
    },

    /// Record marking start of an invocation
    #[serde(rename = "platform.start")]
    PlatformStart {
        /// Request identifier
        request_id: String,
        /// Version of the Lambda function
        #[serde(skip_serializing_if = "Option::is_none")]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        error_type: Option<String>,
        /// Metrics corresponding to the runtime
        #[serde(skip_serializing_if = "Option::is_none")]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        error_type: Option<String>,
        /// Metrics
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
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
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
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum InitPhase {
    /// Initialization phase
    Init,
    /// Invocation phase
    Invoke,
}

/// Status of invocation/initialization
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub enum Status {
    /// Success
    Success,
    /// Error
    Error,
    /// Failure
    Failure,
    /// Timeout
    Timeout,
}

///Init report metrics
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitReportMetrics {
    /// Duration of initialization
    pub duration_ms: f64,
}

/// Runtime done metrics
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDoneMetrics {
    /// Duration in milliseconds
    pub duration_ms: f64,
    /// Number of bytes produced as a result of the invocation
    pub produced_bytes: Option<u64>,
}

/// Report metrics
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
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
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub init_duration_ms: Option<f64>,
    /// Restore duration in milliseconds
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub restore_duration_ms: Option<f64>,
}
