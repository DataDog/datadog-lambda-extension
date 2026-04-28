use serde::Serialize;

pub mod processor;

/// Context extracted from an `aws.lambda` span and forwarded to the logs pipeline.
#[derive(Clone)]
pub struct DurableContextUpdate {
    pub request_id: String,
    pub execution_id: String,
    pub execution_name: String,
    pub first_invocation: Option<bool>,
    pub execution_status: Option<String>,
}

/// Durable execution context stored per `request_id` in `LambdaProcessor::durable_context_map`.
#[derive(Clone, Debug)]
pub struct DurableExecutionContext {
    pub execution_id: String,
    pub execution_name: String,
    pub first_invocation: Option<bool>,
    pub execution_status: Option<String>,
}

///
/// Intake Log for AWS Lambda Telemetry Events.
///
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IntakeLog {
    /// Setting it as a struct, allowing us to override fields.
    pub message: Message,
    pub hostname: String,
    pub service: String,
    #[serde(rename(serialize = "ddtags"))]
    pub tags: String,
    #[serde(rename(serialize = "ddsource"))]
    pub source: String,
}

///
/// Message for AWS Lambda logs.
///
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Message {
    /// The actual log message.
    pub message: String,
    pub lambda: Lambda,
    /// Override the timestamp with the `TelemetryEvent` timestamp.
    pub timestamp: i64,
    pub status: String,
}

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct Lambda {
    pub arn: String,
    pub request_id: Option<String>,
    #[serde(
        rename = "durable_function.execution_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub durable_execution_id: Option<String>,
    #[serde(
        rename = "durable_function.execution_name",
        skip_serializing_if = "Option::is_none"
    )]
    pub durable_execution_name: Option<String>,
    #[serde(
        rename = "durable_function.first_invocation",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_invocation: Option<bool>,
    #[serde(
        rename = "durable_function.execution_status",
        skip_serializing_if = "Option::is_none"
    )]
    pub durable_execution_status: Option<String>,
}

impl Message {
    #[must_use]
    pub fn new(
        message: String,
        request_id: Option<String>,
        function_arn: String,
        timestamp: i64,
        status: Option<String>,
    ) -> Message {
        Message {
            message,
            lambda: Lambda {
                arn: function_arn,
                request_id,
                ..Lambda::default()
            },
            timestamp,
            status: status.unwrap_or("info".to_string()),
        }
    }
}
