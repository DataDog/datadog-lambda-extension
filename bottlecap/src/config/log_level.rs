use std::str::FromStr;

use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tracing::error;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
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

impl FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "error" => Ok(LogLevel::Error),
            "warn" => Ok(LogLevel::Warn),
            "info" => Ok(LogLevel::Info),
            "debug" => Ok(LogLevel::Debug),
            "trace" => Ok(LogLevel::Trace),
            _ => Err(format!(
                "Invalid log level: '{s}'. Valid levels are: error, warn, info, debug, trace",
            )),
        }
    }
}

impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;

        if let Value::String(s) = value {
            match LogLevel::from_str(&s) {
                Ok(level) => Ok(level),
                Err(e) => {
                    error!("{}", e);
                    Ok(LogLevel::Warn)
                }
            }
        } else {
            error!("Expected a string for log level, got {:?}", value);
            Ok(LogLevel::Warn)
        }
    }
}
