use serde::Serialize;

pub mod processor;

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

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Lambda {
    pub arn: String,
    pub request_id: Option<String>,
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
            },
            timestamp,
            status: status.unwrap_or("info".to_string()),
        }
    }
}
