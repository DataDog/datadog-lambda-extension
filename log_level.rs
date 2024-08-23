use serde::{Deserialize, Deserializer};
use tracing::error;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Default)]
pub enum LogLevel {
    /// Designates very serious errors.
    Error,
    /// Designates hazardous situations.
    #[default]
    Warn,
    /// Designates useful information.
    Info,
    /// Designates lower priority information.
    Debug,
    /// Designates very low priority, often extremely verbose, information.
    Trace,
}

impl AsRef<str> for LogLevel {
    fn as_ref(&self) -> &str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
        }
    }
}

impl LogLevel {
    /// Construct a `log::LevelFilter` from a `LogLevel`
    #[must_use]
    pub fn as_level_filter(self) -> log::LevelFilter {
        match self {
            LogLevel::Error => log::LevelFilter::Error,
            LogLevel::Warn => log::LevelFilter::Warn,
            LogLevel::Info => log::LevelFilter::Info,
            LogLevel::Debug => log::LevelFilter::Debug,
            LogLevel::Trace => log::LevelFilter::Trace,
        }
    }
}

#[allow(clippy::module_name_repetitions)]
pub fn deserialize_log_level<'de, D>(deserializer: D) -> Result<LogLevel, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    match s.to_lowercase().as_str() {
        "error" => Ok(LogLevel::Error),
        "warn" => Ok(LogLevel::Warn),
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        _ => {
            error!("Unknown log level: {}, using warn", s);
            Ok(LogLevel::Warn)
        }
    }
}
