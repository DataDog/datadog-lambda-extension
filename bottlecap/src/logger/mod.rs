use std::fmt::{self, Write as _};
use tracing_core::{Event, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::fmt::{
    FmtContext, FormattedFields,
    format::{self, FormatEvent, FormatFields},
};
use tracing_subscriber::registry::LookupSpan;

pub mod cold_start;

// `DD_LOG_LEVEL_BY_TARGET` matches a target by text prefix, so no target name may start with
// another one: an entry for `trace_flush` would also match `trace_flush_duration`.
// `README.md` in this folder documents `DD_LOG_LEVEL_BY_TARGET`.

/// Tracing target for the duration of each cold start step.
pub const COLD_START_DURATION: &str = "cold_start_duration";

/// HTTP libraries that log each request, including the requests that send the extension's own
/// logs to Datadog. The extension does not send their lines to Datadog, so that a flush cannot
/// log lines for the next flush to send.
pub const HTTP_LIBRARY_TARGETS: &[&str] = &["h2", "hyper", "reqwest", "rustls"];

/// Starts each line that the formatter writes for an [`HTTP_LIBRARY_TARGETS`] target.
const HTTP_LIBRARY_LINE_PREFIX: &str = "{\"target\":\"";

fn is_http_library_target(target: &str) -> bool {
    HTTP_LIBRARY_TARGETS
        .iter()
        .any(|library| target.starts_with(library))
}

/// Returns true if `record`, an `extension` record from the Telemetry API, is a line that the
/// formatter wrote for an [`HTTP_LIBRARY_TARGETS`] target. The message check keeps the lines of
/// other extensions.
#[must_use]
pub fn is_http_library_record(record: &serde_json::Value) -> bool {
    const MESSAGE_PREFIX: &str = "DD_EXTENSION | ";
    match record {
        serde_json::Value::String(line) => {
            line.starts_with(HTTP_LIBRARY_LINE_PREFIX)
                && line.contains(&format!(",\"message\":\"{MESSAGE_PREFIX}"))
        }
        serde_json::Value::Object(obj) => {
            obj.contains_key("target")
                && obj
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|message| message.starts_with(MESSAGE_PREFIX))
        }
        _ => false,
    }
}

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

/// Visitor that captures the message and the `f64` fields of a tracing event, so that Datadog
/// parses those fields as numbers. It keeps the other fields as `name=value` text, which only
/// HTTP library lines write. To write another type, add its `record_*` method.
#[derive(Default)]
struct FieldVisitor {
    message: String,
    /// The `f64` fields, in the order of the event.
    fields: Vec<(&'static str, f64)>,
    /// The other fields as ` name=value` text. h2, for example, logs each frame in a `frame` field.
    other_fields: String,
}

impl FieldVisitor {
    fn record_other(&mut self, field: &tracing::field::Field, value: fmt::Arguments<'_>) {
        // `log.*` fields carry the target and location of a record from the `log` crate.
        if !field.name().starts_with("log.") {
            let _ = write!(self.other_fields, " {}={value}", field.name());
        }
    }
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.record_other(field, format_args!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.record_other(field, format_args!("{value}"));
        }
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        // JSON has no NaN or infinity, so drop those values.
        if value.is_finite() {
            self.fields.push((field.name(), value));
        }
    }
}

/// Groups `fields` into JSON members. A field named `group.key` goes into a nested
/// `"group": {"key": ...}` object.
fn fields_to_json(fields: &[(&'static str, f64)]) -> serde_json::Map<String, serde_json::Value> {
    let mut members = serde_json::Map::new();
    for (name, value) in fields {
        match name.split_once('.') {
            None => {
                members.insert((*name).to_string(), (*value).into());
            }
            Some((group, key)) => {
                if let serde_json::Value::Object(group) = members
                    .entry(group)
                    .or_insert_with(|| serde_json::Map::new().into())
                {
                    group.insert(key.to_string(), (*value).into());
                }
            }
        }
    }
    members
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
        // A record from the `log` crate arrives with the target `log`, and its own target in a field.
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());
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

        let is_http_library = is_http_library_target(metadata.target());
        let other_fields = if is_http_library {
            visitor.other_fields.as_str()
        } else {
            ""
        };
        let message = format!(
            "DD_EXTENSION | {level} | {span_prefix}{}{other_fields}",
            visitor.message
        );

        // Setting specifically `status` as opposed to `level` since AWS
        // filters out logs by the `level` field. This allows our logs to
        // appear in CWL regardless of the log level.
        writer.write_char('{')?;
        if is_http_library {
            writer.write_str("\"target\":\"")?;
            write_json_escaped(&mut writer, metadata.target())?;
            writer.write_str("\",")?;
        }
        write!(writer, "\"status\":\"{level}\",\"message\":\"")?;
        write_json_escaped(&mut writer, &message)?;
        writer.write_char('"')?;
        for (key, value) in fields_to_json(&visitor.fields) {
            write!(writer, ",{}:{value}", serde_json::Value::from(key))?;
        }
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
    fn test_formatter_writes_only_finite_f64_fields() {
        let output = capture_log(|| {
            tracing::info!(
                ms = 1.5_f64,
                nan = f64::NAN,
                count = 3_u64,
                name = "x",
                "fields"
            );
        });

        let parsed: serde_json::Value =
            serde_json::from_str(output.trim()).expect("output should be valid JSON");

        assert_eq!(parsed["ms"].as_f64(), Some(1.5));
        assert!(parsed.get("nan").is_none());
        assert!(parsed.get("count").is_none());
        assert!(parsed.get("name").is_none());
    }

    #[test]
    fn test_formatter_starts_http_library_line_with_target() {
        let output = capture_log(|| {
            tracing::debug!(
                target: "hyper_util::client::legacy::connect::http",
                "connecting to 34.149.115.168:443"
            );
        });

        assert!(output.starts_with(
            r#"{"target":"hyper_util::client::legacy::connect::http","status":"DEBUG","#
        ));
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert!(is_http_library_record(&serde_json::Value::String(
            output.trim().to_string()
        )));
        assert!(is_http_library_record(&parsed));
    }

    #[test]
    fn test_formatter_uses_target_of_log_crate_record() {
        let output = capture_log(|| {
            tracing_log::format_trace(
                &log::Record::builder()
                    .target("rustls::client::hs")
                    .level(log::Level::Debug)
                    .args(format_args!("Not resuming any session"))
                    .build(),
            )
            .unwrap();
        });

        assert!(output.starts_with(r#"{"target":"rustls::client::hs","status":"DEBUG","#));
        assert!(output.contains(r#"DD_EXTENSION | DEBUG | Not resuming any session""#));
    }

    #[test]
    fn test_formatter_writes_other_fields_only_for_http_library_lines() {
        let output = capture_log(|| {
            tracing::debug!(target: "h2::codec::framed_write", frame = ?("Headers", 1), "send");
            tracing::debug!(target: "bottlecap::traces", attempt = 1_u64, "Attempting request");
        });
        let lines: Vec<&str> = output.lines().collect();

        assert!(lines[0].contains(r#"DD_EXTENSION | DEBUG | send frame=(\"Headers\", 1)""#));
        assert!(lines[1].contains(r#"DD_EXTENSION | DEBUG | Attempting request""#));
    }

    #[test]
    fn test_formatter_writes_no_target_for_extension_line() {
        let output = capture_log(|| {
            tracing::debug!(target: "bottlecap::traces", "Flushing 1 traces");
        });

        assert!(output.starts_with(r#"{"status":"DEBUG","#));
        assert!(!is_http_library_record(&serde_json::Value::String(
            output.trim().to_string()
        )));
    }

    #[test]
    fn test_is_http_library_record_keeps_other_extension_lines() {
        let other_extension = serde_json::json!({"target": "app", "message": "started"});
        assert!(!is_http_library_record(&other_extension));
        assert!(!is_http_library_record(&serde_json::Value::String(
            r#"{"target":"app","message":"started"}"#.to_string()
        )));
        assert!(!is_http_library_record(&serde_json::Value::String(
            "plain text line".to_string()
        )));
    }
}
