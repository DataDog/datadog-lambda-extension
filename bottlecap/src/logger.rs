use std::fmt;
use tracing_core::{Event, Subscriber};
use tracing_subscriber::fmt::{
    FmtContext, FormattedFields,
    format::{self, FormatEvent, FormatFields},
};
use tracing_subscriber::registry::LookupSpan;

#[derive(Debug, Clone, Copy)]
pub struct Formatter;

/// Visitor that captures the message from tracing event fields.
struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_string();
        }
    }
}

impl<S, N> FormatEvent<S, N> for Formatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let level = metadata.level();

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        // Build span context prefix
        let mut span_prefix = String::new();
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                span_prefix.push_str(span.name());
                let ext = span.extensions();
                if let Some(fields) = ext.get::<FormattedFields<N>>()
                    && !fields.is_empty()
                {
                    span_prefix.push('{');
                    span_prefix.push_str(fields);
                    span_prefix.push('}');
                }
                span_prefix.push_str(": ");
            }
        }

        let message = format!("DD_EXTENSION | {level} | {span_prefix}{}", visitor.0);

        // Use serde_json for safe serialization (handles escaping automatically)
        let output = serde_json::json!({
            "level": level.to_string(),
            "message": message,
        });

        writeln!(writer, "{output}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tracing::subscriber::with_default;
    use tracing_subscriber::fmt::Subscriber;

    /// Captures all output from a tracing subscriber using our `Formatter`.
    fn capture_log<F: FnOnce()>(f: F) -> String {
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let buf_clone = buf.clone();

        let subscriber = Subscriber::builder()
            .with_writer(move || -> Box<dyn std::io::Write + Send> {
                Box::new(WriterGuard(buf_clone.clone()))
            })
            .with_max_level(tracing::Level::TRACE)
            .with_level(true)
            .with_target(false)
            .without_time()
            .event_format(Formatter)
            .finish();

        with_default(subscriber, f);

        let lock = buf.lock().expect("test lock poisoned");
        String::from_utf8(lock.clone()).expect("invalid UTF-8 in log output")
    }

    struct WriterGuard(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for WriterGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("write lock poisoned").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_formatter_outputs_valid_json_with_level() {
        let output = capture_log(|| {
            tracing::info!("hello world");
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");

        assert_eq!(parsed["level"], "INFO");
        assert!(
            parsed["message"]
                .as_str()
                .unwrap()
                .contains("DD_EXTENSION | INFO | hello world")
        );
    }

    #[test]
    fn test_formatter_error_level() {
        let output = capture_log(|| {
            tracing::error!("something broke");
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");
        assert_eq!(parsed["level"], "ERROR");
        assert!(
            parsed["message"]
                .as_str()
                .unwrap()
                .contains("DD_EXTENSION | ERROR | something broke")
        );
    }

    #[test]
    fn test_formatter_debug_level() {
        let output = capture_log(|| {
            tracing::debug!("debug details");
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");
        assert_eq!(parsed["level"], "DEBUG");
        assert!(
            parsed["message"]
                .as_str()
                .unwrap()
                .contains("DD_EXTENSION | DEBUG | debug details")
        );
    }

    #[test]
    fn test_formatter_escapes_special_characters() {
        let output = capture_log(|| {
            tracing::info!("message with \"quotes\" and a\nnewline");
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("special chars should be escaped");
        let msg = parsed["message"].as_str().unwrap();
        assert!(msg.contains("quotes"));
        assert!(msg.contains("newline"));
    }
}
