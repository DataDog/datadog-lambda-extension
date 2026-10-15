use tokio::time::Instant;

use super::COLD_START_DURATION;

/// Duration of each cold start step in milliseconds. The steps go in one log line as numeric
/// fields, so that Datadog can graph each step.
#[derive(Debug, Clone, Copy)]
pub struct ColdStartDurations {
    /// When the extension process started.
    pub start_time: Instant,
    pub enable_logging_ms: f64,
    pub load_aws_config_ms: f64,
    pub create_extensions_api_client_ms: f64,
    pub register_extension_ms: f64,
    pub load_config_ms: f64,
    pub resolve_additional_endpoint_secrets_ms: f64,
    pub wait_for_register_ms: f64,
    pub start_dogstatsd_ms: f64,
    pub create_appsec_processor_ms: f64,
    pub start_telemetry_listener_ms: f64,
}

impl ColdStartDurations {
    #[must_use]
    pub fn new(start_time: Instant) -> Self {
        Self {
            start_time,
            enable_logging_ms: 0.0,
            load_aws_config_ms: 0.0,
            create_extensions_api_client_ms: 0.0,
            register_extension_ms: 0.0,
            load_config_ms: 0.0,
            resolve_additional_endpoint_secrets_ms: 0.0,
            wait_for_register_ms: 0.0,
            start_dogstatsd_ms: 0.0,
            create_appsec_processor_ms: 0.0,
            start_telemetry_listener_ms: 0.0,
        }
    }

    /// Logs all durations in one line, with the time since `start_time` as the total.
    pub fn log(&self) {
        // `debug!` cannot parse dotted field names after `target:`, so this uses `event!`.
        tracing::event!(
            target: COLD_START_DURATION,
            tracing::Level::DEBUG,
            cold_start.total_ms = elapsed_ms(self.start_time),
            cold_start.enable_logging_ms = self.enable_logging_ms,
            cold_start.load_aws_config_ms = self.load_aws_config_ms,
            cold_start.create_extensions_api_client_ms = self.create_extensions_api_client_ms,
            cold_start.register_extension_ms = self.register_extension_ms,
            cold_start.load_config_ms = self.load_config_ms,
            cold_start.resolve_additional_endpoint_secrets_ms =
                self.resolve_additional_endpoint_secrets_ms,
            cold_start.wait_for_register_ms = self.wait_for_register_ms,
            cold_start.start_dogstatsd_ms = self.start_dogstatsd_ms,
            cold_start.create_appsec_processor_ms = self.create_appsec_processor_ms,
            cold_start.start_telemetry_listener_ms = self.start_telemetry_listener_ms,
            "Cold start step durations"
        );
    }
}

/// Milliseconds since `start_time`, rounded to 2 decimals.
#[must_use]
pub fn elapsed_ms(start_time: Instant) -> f64 {
    (start_time.elapsed().as_secs_f64() * 100_000.0).round() / 100.0
}
