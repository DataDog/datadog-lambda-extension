//! The one simulated invocation that every generated fixture describes: a cold start of a
//! Node.js 22 function, traced by the Node.js tracer. Fixtures take every identifying value
//! from here, and `fixtures_corpus` checks that they agree.

/// Parts of [`FUNCTION_ARN`].
pub const FUNCTION_NAME: &str = "orders-prod-handler";
pub const REGION: &str = "us-east-1";
pub const ACCOUNT_ID: &str = "123456789012";

/// The unqualified ARN of [`FUNCTION_NAME`]; the invocation runs `$LATEST`.
pub const FUNCTION_ARN: &str = "arn:aws:lambda:us-east-1:123456789012:function:orders-prod-handler";
pub const FUNCTION_VERSION: &str = "$LATEST";
pub const MEMORY_SIZE_MB: u32 = 128;
pub const ARCHITECTURE: &str = "arm64";
pub const INIT_TYPE: &str = "on-demand";
/// The Lambda runtime, and the version `platform.initStart` reports for it.
pub const RUNTIME: &str = "nodejs22.x";
pub const RUNTIME_VERSION: &str = "nodejs:22.v10";

/// `DD_SERVICE`, `DD_ENV` and `DD_VERSION` of the function.
pub const SERVICE: &str = "orders";
pub const ENV: &str = "prod";
pub const VERSION: &str = "1.2.3";

pub const REQUEST_ID: &str = "8f5c1b9e-3a2d-4c6f-9e1b-7d2a5c8e4f10";

/// X-Ray root trace id of the invocation (active tracing).
pub const XRAY_ROOT: &str = "1-62e900b2-710d76f009d6e7785905449a";

/// X-Ray trace header with [`XRAY_ROOT`] that `platform.start` and the Runtime API carry.
pub const XRAY_TRACE_HEADER: &str =
    "Root=1-62e900b2-710d76f009d6e7785905449a;Parent=0efbd19962d95b05;Sampled=1";

/// The Node.js tracer that sends the trace fixtures.
pub const TRACER_LANG: &str = "nodejs";
pub const TRACER_LANG_VERSION: &str = "v22.11.0";
pub const TRACER_INTERPRETER: &str = "v8";
pub const TRACER_VERSION: &str = "5.40.0";

/// 2026-10-27T12:00:00Z: `platform.initStart`, and the send time of the generated trigger
/// events. Every other time is an offset from it, in milliseconds.
pub const BASE_NS: i64 = 1_793_102_400_000_000_000;

/// `platform.start`.
pub const START_MS: usize = 50;
/// The handler: the `aws.lambda` root span and the `responseLatency` span.
pub const HANDLER_START_MS: usize = 51;
pub const HANDLER_MS: usize = 90;
/// `platform.runtimeDone`.
pub const RUNTIME_DONE_MS: usize = 150;

/// `millis` in nanoseconds.
pub const fn millis_to_ns(millis: usize) -> i64 {
    millis as i64 * 1_000_000
}

/// The instant `millis` after [`BASE_NS`], as the Telemetry API writes `time`.
pub fn time(millis: usize) -> String {
    chrono::DateTime::from_timestamp_nanos(BASE_NS + millis_to_ns(millis))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Time of the `i`th function log line: nondecreasing, 12 lines per millisecond, from 10 ms
/// after `platform.start` to just before `platform.runtimeDone`.
pub fn log_millis(i: usize) -> usize {
    START_MS + 10 + (i / 12).min(RUNTIME_DONE_MS - START_MS - 11)
}
