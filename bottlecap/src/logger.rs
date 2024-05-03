use crate::config::LogLevel;
use log::{LevelFilter, Log, Metadata, Record};

pub struct SimpleLogger {}

impl SimpleLogger {
    pub fn init(level: LogLevel) -> Result<(), log::SetLoggerError> {
        let level = match level {
            LogLevel::Trace => LevelFilter::Trace,
            LogLevel::Debug => LevelFilter::Debug,
            LogLevel::Info => LevelFilter::Info,
            LogLevel::Warn => LevelFilter::Warn,
            LogLevel::Error => LevelFilter::Error,
        };

        log::set_max_level(level);
        log::set_logger(&SimpleLogger {})
    }
}

impl Log for SimpleLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        println!("[{}] {}", record.level(), record.args());
    }

    fn flush(&self) {}
}
