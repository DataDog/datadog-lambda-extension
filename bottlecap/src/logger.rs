use std::fmt;
use tracing_core::{Event, Subscriber};
use tracing_subscriber::fmt::{
    FmtContext, FormattedFields,
    format::{self, FormatEvent, FormatFields},
};
use tracing_subscriber::registry::LookupSpan;

/// Tracing target for the duration of each cold start step.
pub const COLD_START_DURATION: &str = "cold_start_duration";

/// Writes `s` to `w` with the 6 mandatory JSON string escape sequences applied.
/// Handles: `"`, `\`, `\n`, `\r`, `\t`, and U+0000–U+001F control characters.
fn write_json_escaped(w: &mut impl fmt::Write, s: &str) -> fmt::Result {
    for c in s.chars() {
        match c {
            '"' => w.write_str("\\\"")?,
            '\\' => w.write_str("\\\\")?,
            '\n' => w.write_str("\\n")?,
            '\r' => w.write_str("\\r")?,
            '\t' => w.write_str("\\t")?,
            c if (c as u32) < 0x20 => write!(w, "\\u{:04X}", c as u32)?,
            c => w.write_char(c)?,
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct Formatter;

/// Visitor that captures the message and the other fields of a tracing event. Each other field
/// becomes a JSON value, so that Datadog parses numbers as numbers.
#[derive(Default)]
struct FieldVisitor {
    message: String,
    /// Field names and JSON values, in the order of the event.
    fields: Vec<(&'static str, String)>,
}

impl FieldVisitor {
    fn push_string(&mut self, field: &tracing::field::Field, value: &str) {
        let mut json = String::from("\"");
        // Writing to a `String` cannot fail.
        let _ = write_json_escaped(&mut json, value);
        json.push('"');
        self.fields.push((field.name(), json));
    }
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.push_string(field, &format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.push_string(field, value);
        }
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        // JSON has no NaN or infinity, so write those as strings.
        if value.is_finite() {
            self.fields.push((field.name(), value.to_string()));
        } else {
            self.push_string(field, &value.to_string());
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.push((field.name(), value.to_string()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.push((field.name(), value.to_string()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.push((field.name(), value.to_string()));
    }
}

/// Writes `fields` as JSON members, each one preceded by a comma. A field named `group.key`
/// goes into a nested `"group": {"key": ...}` object.
fn write_json_fields(w: &mut impl fmt::Write, fields: &[(&'static str, String)]) -> fmt::Result {
    let mut written_groups: Vec<&str> = Vec::new();
    for (name, value) in fields {
        match name.split_once('.') {
            None => {
                w.write_str(",\"")?;
                write_json_escaped(w, name)?;
                write!(w, "\":{value}")?;
            }
            Some((group, _)) => {
                if written_groups.contains(&group) {
                    continue;
                }
                written_groups.push(group);
                w.write_str(",\"")?;
                write_json_escaped(w, group)?;
                w.write_str("\":{")?;
                let members = fields.iter().filter_map(|(n, v)| {
                    n.split_once('.')
                        .filter(|(g, _)| *g == group)
                        .map(|(_, k)| (k, v))
                });
                for (i, (key, value)) in members.enumerate() {
                    w.write_str(if i == 0 { "\"" } else { ",\"" })?;
                    write_json_escaped(w, key)?;
                    write!(w, "\":{value}")?;
                }
                w.write_str("}")?;
            }
        }
    }
    Ok(())
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

        let mut visitor = FieldVisitor::default();
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

        let message = format!("DD_EXTENSION | {level} | {span_prefix}{}", visitor.message);

        // Setting specifically `status` as opposed to `level` since AWS
        // filters out logs by the `level` field. This allows our logs to
        // appear in CWL regardless of the log level.
        write!(writer, "{{\"status\":\"{level}\",\"message\":\"")?;
        write_json_escaped(&mut writer, &message)?;
        writer.write_char('"')?;
        write_json_fields(&mut writer, &visitor.fields)?;
        writeln!(writer, "}}")
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
            self.0
                .lock()
                .expect("write lock poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn escaped(s: &str) -> String {
        let mut out = String::new();
        write_json_escaped(&mut out, s).expect("write_json_escaped failed");
        out
    }

    #[test]
    fn test_escape_plain_text_is_unchanged() {
        assert_eq!(escaped("hello world"), "hello world");
    }

    #[test]
    fn test_escape_double_quote() {
        assert_eq!(escaped(r#"say "hi""#), r#"say \"hi\""#);
    }

    #[test]
    fn test_escape_backslash() {
        assert_eq!(escaped(r"C:\path"), r"C:\\path");
    }

    #[test]
    fn test_escape_newline() {
        assert_eq!(escaped("line1\nline2"), r"line1\nline2");
    }

    #[test]
    fn test_escape_carriage_return() {
        assert_eq!(escaped("a\rb"), r"a\rb");
    }

    #[test]
    fn test_escape_tab() {
        assert_eq!(escaped("a\tb"), r"a\tb");
    }

    #[test]
    fn test_escape_control_characters() {
        // U+0001 (SOH) and U+001F (US) must be \uXXXX-escaped
        assert_eq!(escaped("\x01"), r"\u0001");
        assert_eq!(escaped("\x1F"), r"\u001F");
    }

    #[test]
    fn test_escape_unicode_above_control_range_passes_through() {
        // U+0020 (space) and above are not escaped
        assert_eq!(escaped("€ ñ 中"), "€ ñ 中");
    }

    #[test]
    fn test_formatter_outputs_valid_json_with_level() {
        let output = capture_log(|| {
            tracing::info!("hello world");
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");

        assert_eq!(parsed["status"], "INFO");
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
        assert_eq!(parsed["status"], "ERROR");
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
        assert_eq!(parsed["status"], "DEBUG");
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

        // The raw output must contain escaped quotes and newlines to be valid JSON
        assert!(
            output.contains(r#"\"quotes\""#),
            "quotes should be escaped in raw JSON"
        );
        assert!(
            output.contains(r"\n"),
            "newline should be escaped in raw JSON"
        );

        // And it must parse as valid JSON
        let parsed: serde_json::Value = serde_json::from_str(output.trim())
            .expect("output with special chars should be valid JSON");
        let msg = parsed["message"]
            .as_str()
            .expect("message field should be a string");
        assert!(
            msg.contains("\"quotes\""),
            "parsed message should contain literal quotes"
        );
        assert!(
            msg.contains('\n'),
            "parsed message should contain literal newline"
        );
    }

    #[test]
    fn test_formatter_nests_grouped_fields_as_numbers() {
        let output = capture_log(|| {
            tracing::event!(
                tracing::Level::DEBUG,
                cold_start.total_ms = 21.0_f64,
                cold_start.load_config_ms = 1.61_f64,
                "durations"
            );
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");

        assert_eq!(parsed["cold_start"]["total_ms"].as_f64(), Some(21.0));
        assert_eq!(parsed["cold_start"]["load_config_ms"].as_f64(), Some(1.61));
        assert!(
            parsed["message"]
                .as_str()
                .unwrap()
                .contains("DD_EXTENSION | DEBUG | durations")
        );
    }

    #[test]
    fn test_formatter_omits_none_fields() {
        let output = capture_log(|| {
            tracing::event!(
                tracing::Level::DEBUG,
                cold_start.secrets_ms = None::<f64>,
                cold_start.load_config_ms = 1.5_f64,
                "durations"
            );
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");

        assert!(parsed["cold_start"].get("secrets_ms").is_none());
        assert_eq!(parsed["cold_start"]["load_config_ms"].as_f64(), Some(1.5));
    }

    #[test]
    fn test_formatter_writes_flat_fields_by_type() {
        let output = capture_log(|| {
            tracing::info!(count = 3_u64, ok = true, name = "a \"b\"", "fields");
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");

        assert_eq!(parsed["count"].as_u64(), Some(3));
        assert_eq!(parsed["ok"].as_bool(), Some(true));
        assert_eq!(parsed["name"].as_str(), Some("a \"b\""));
    }
}
