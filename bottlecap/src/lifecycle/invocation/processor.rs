use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use datadog_trace_protobuf::pb::Span;
use datadog_trace_utils::{send_data::SendData, tracer_header_tags};
use dogstatsd::aggregator::Aggregator as MetricsAggregator;
use serde_json::{json, Value};
use tokio::sync::{mpsc::Sender, watch};
use tracing::debug;

use crate::{
    config::{self, AwsConfig},
    lifecycle::invocation::{
        base64_to_string, context::ContextBuffer, create_empty_span, generate_span_id,
        span_inferrer::SpanInferrer, tag_span_from_value,
    },
    metrics::enhanced::lambda::{EnhancedMetricData, Lambda as EnhancedMetrics},
    proc::{
        self,
        constants::{ETC_PATH, PROC_PATH},
        CPUData, NetworkData,
    },
    tags::{lambda::tags::resolve_runtime_from_proc, provider},
    telemetry::events::{ReportMetrics, RuntimeDoneMetrics, Status},
    traces::{
        context::SpanContext,
        propagation::{
            text_map_propagator::{
                DatadogHeaderPropagator, DATADOG_PARENT_ID_KEY, DATADOG_SAMPLING_PRIORITY_KEY,
                DATADOG_SPAN_ID_KEY, DATADOG_TRACE_ID_KEY,
            },
            DatadogCompositePropagator, Propagator,
        },
        trace_processor,
    },
};

pub const MS_TO_NS: f64 = 1_000_000.0;
pub const S_TO_NS: f64 = 1_000_000_000.0;
pub const PROACTIVE_INITIALIZATION_THRESHOLD_MS: u64 = 10_000;

pub const DATADOG_INVOCATION_ERROR_MESSAGE_KEY: &str = "x-datadog-invocation-error-msg";
pub const DATADOG_INVOCATION_ERROR_TYPE_KEY: &str = "x-datadog-invocation-error-type";
pub const DATADOG_INVOCATION_ERROR_STACK_KEY: &str = "x-datadog-invocation-error-stack";
pub const DATADOG_INVOCATION_ERROR_KEY: &str = "x-datadog-invocation-error";
const TAG_SAMPLING_PRIORITY: &str = "_sampling_priority_v1";

pub struct Processor {
    // Buffer containing context of the previous 5 invocations
    pub context_buffer: Arc<Mutex<ContextBuffer>>,
    // Helper to infer span information
    inferrer: SpanInferrer,
    // Current invocation span
    pub span: Span,
    // Cold start span
    cold_start_span: Option<Span>,
    // Extracted span context from inferred span, headers, or payload
    pub extracted_span_context: Option<SpanContext>,
    // Used to extract the trace context from inferred span, headers, or payload
    propagator: DatadogCompositePropagator,
    // Helper to send enhanced metrics
    enhanced_metrics: EnhancedMetrics,
    // AWS configuration from the Lambda environment
    aws_config: AwsConfig,
    // Flag to determine if a tracer was detected
    tracer_detected: bool,
    runtime: Option<String>,
    config: Arc<config::Config>,
}

impl Processor {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
        aws_config: &AwsConfig,
        metrics_aggregator: Arc<Mutex<MetricsAggregator>>,
        context_buffer: Arc<Mutex<ContextBuffer>>,
    ) -> Self {
        let service = config.service.clone().unwrap_or(String::from("aws.lambda"));
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or(String::from("aws.lambda"));

        let propagator = DatadogCompositePropagator::new(Arc::clone(&config));

        Processor {
            context_buffer,
            inferrer: SpanInferrer::new(config.service_mapping.clone()),
            span: create_empty_span(String::from("aws.lambda"), resource, service),
            cold_start_span: None,
            extracted_span_context: None,
            propagator,
            enhanced_metrics: EnhancedMetrics::new(metrics_aggregator, Arc::clone(&config)),
            aws_config: aws_config.clone(),
            tracer_detected: false,
            runtime: None,
            config: Arc::clone(&config),
        }
    }

    /// Given a `request_id`, creates the context and adds the enhanced metric offsets to the context buffer.
    ///
    pub fn on_invoke_event(&mut self, request_id: String) {
        let timestamp = std::time::UNIX_EPOCH
            .elapsed()
            .expect("can't poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        self.reset_state();
        self.set_init_tags();

        let mut context_buffer = self.context_buffer.lock().expect("lock poisoned");
        context_buffer.create_context(&request_id);
        if self.config.enhanced_metrics {
            // Collect offsets for network and cpu metrics
            let network_offset: Option<NetworkData> = proc::get_network_data().ok();
            let cpu_offset: Option<CPUData> = proc::get_cpu_data().ok();
            let uptime_offset: Option<f64> = proc::get_uptime().ok();

            // Start a channel for monitoring tmp enhanced data
            let (tmp_chan_tx, tmp_chan_rx) = watch::channel(());
            self.enhanced_metrics.set_tmp_enhanced_metrics(tmp_chan_rx);

            // Start a channel for monitoring file descriptor and thread count
            let (process_chan_tx, process_chan_rx) = watch::channel(());
            self.enhanced_metrics
                .set_process_enhanced_metrics(process_chan_rx);

            let enhanced_metric_offsets = Some(EnhancedMetricData {
                network_offset,
                cpu_offset,
                uptime_offset,
                tmp_chan_tx,
                process_chan_tx,
            });
            context_buffer.add_enhanced_metric_data(&request_id, enhanced_metric_offsets);
        }
        drop(context_buffer);

        // Increment the invocation metric
        self.enhanced_metrics.increment_invocation_metric(timestamp);
    }

    /// Resets the state of the processor to default values.
    ///
    fn reset_state(&mut self) {
        // Reset Span Context on Span
        self.span.trace_id = 0;
        self.span.parent_id = 0;
        self.span.span_id = 0;
        // Error
        self.span.error = 0;
        // Meta tags
        self.span.meta.clear();
        // Extracted Span Context
        self.extracted_span_context = None;
        // Cold Start Span
        self.cold_start_span = None;
    }

    /// On the first invocation, determine if it's a cold start or proactive init.
    ///
    /// For every other invocation, it will always be warm start.
    ///
    fn set_init_tags(&mut self) {
        let mut proactive_initialization = false;
        let mut cold_start = false;

        // If it's empty, then we are in a cold start
        let context_buffer = self.context_buffer.lock().expect("lock poisoned");
        if context_buffer.is_empty() {
            let now = Instant::now();
            let time_since_sandbox_init = now.duration_since(self.aws_config.sandbox_init_time);
            if time_since_sandbox_init.as_millis() > PROACTIVE_INITIALIZATION_THRESHOLD_MS.into() {
                proactive_initialization = true;
            } else {
                cold_start = true;
            }

            // Resolve runtime only once
            let runtime = resolve_runtime_from_proc(PROC_PATH, ETC_PATH);
            self.enhanced_metrics.set_runtime_tag(&runtime);
            self.runtime = Some(runtime);
        }
        drop(context_buffer);

        if proactive_initialization {
            self.span.meta.insert(
                String::from("proactive_initialization"),
                proactive_initialization.to_string(),
            );
        }

        if let Some(runtime) = &self.runtime {
            self.span
                .meta
                .insert(String::from("runtime"), runtime.to_string());
        }

        self.span
            .meta
            .insert(String::from("cold_start"), cold_start.to_string());

        self.enhanced_metrics
            .set_init_tags(proactive_initialization, cold_start);
    }

    pub fn on_platform_init_start(&mut self, time: DateTime<Utc>) {
        // Create a cold start span
        let mut cold_start_span = create_empty_span(
            String::from("aws.lambda.cold_start"),
            self.span.resource.clone(),
            self.span.service.clone(),
        );

        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();

        cold_start_span.span_id = generate_span_id();
        cold_start_span.start = start_time;

        self.cold_start_span = Some(cold_start_span);
    }

    /// Given the duration of the platform init report, set the init duration metric.
    ///
    #[allow(clippy::cast_possible_truncation)]
    pub fn on_platform_init_report(&mut self, duration_ms: f64, timestamp: i64) {
        self.enhanced_metrics
            .set_init_duration_metric(duration_ms, timestamp);

        if let Some(cold_start_span) = &mut self.cold_start_span {
            // `round` is intentionally meant to be a whole integer
            cold_start_span.duration = (duration_ms * MS_TO_NS) as i64;
        }
    }

    /// Given a `request_id` and the time of the platform start, add the start time to the context buffer.
    ///
    /// Also, set the start time of the current span.
    ///
    pub fn on_platform_start(&mut self, request_id: String, time: DateTime<Utc>) {
        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();
        let mut context_buffer = self.context_buffer.lock().expect("lock poisoned");
        context_buffer.add_start_time(&request_id, start_time);
        self.span.start = start_time;
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::await_holding_lock)] // we are dropping the lock before awaiting
    pub async fn on_platform_runtime_done(
        &mut self,
        request_id: &String,
        metrics: RuntimeDoneMetrics,
        status: Status,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_agent_tx: Sender<SendData>,
        timestamp: i64,
    ) {
        let mut context_buffer = self.context_buffer.lock().expect("lock poisoned");
        context_buffer.add_runtime_duration(request_id, metrics.duration_ms);

        // Set the runtime duration metric
        self.enhanced_metrics
            .set_runtime_done_metrics(&metrics, timestamp);

        if status != Status::Success {
            // Increment the error metric
            self.enhanced_metrics.increment_errors_metric(timestamp);

            // Increment the error type metric
            if status == Status::Timeout {
                self.enhanced_metrics.increment_timeout_metric(timestamp);

                // Invocation Span will never have the `trace_id` or `span_id` set
                self.span.trace_id = generate_span_id();
                self.span.span_id = generate_span_id();
                self.span.error = 1;
                self.span.meta.insert(
                    "error.msg".to_string(),
                    "Datadog detected an Impending Timeout".to_string(),
                );
                self.span
                    .meta
                    .insert("error.type".to_string(), "Impending Timeout".to_string());
            }
        }

        if let Some(context) = context_buffer.get(request_id) {
            // `round` is intentionally meant to be a whole integer
            self.span.duration = (context.runtime_duration_ms * MS_TO_NS).round() as i64;
            self.span
                .meta
                .insert("request_id".to_string(), request_id.clone());
            // todo(duncanista): add missing tags
            // - metrics tags (for asm)

            if let Some(tracer_span) = &context.tracer_span {
                self.span.meta.extend(tracer_span.meta.clone());
                self.span
                    .meta_struct
                    .extend(tracer_span.meta_struct.clone());
                self.span.metrics.extend(tracer_span.metrics.clone());
            }

            if let Some(offsets) = &context.enhanced_metric_data {
                self.enhanced_metrics.set_cpu_utilization_enhanced_metrics(
                    offsets.cpu_offset.clone(),
                    offsets.uptime_offset,
                );
                // Send the signal to stop monitoring tmp
                _ = offsets.tmp_chan_tx.send(());
                // Send the signal to stop monitoring file descriptors and threads
                _ = offsets.process_chan_tx.send(());
            }
        }
        // Drop the context buffer lock before awaiting
        drop(context_buffer);

        if let Some(trigger_tags) = self.inferrer.get_trigger_tags() {
            self.span.meta.extend(trigger_tags);
        }

        self.inferrer.complete_inferred_spans(&self.span);

        if let Some(cold_start_span) = &mut self.cold_start_span {
            cold_start_span.trace_id = self.span.trace_id;
            cold_start_span.parent_id = self.span.parent_id;
        }

        if self.tracer_detected {
            let mut body_size = std::mem::size_of_val(&self.span);
            let mut traces = vec![self.span.clone()];

            if let Some(inferred_span) = &self.inferrer.inferred_span {
                body_size += std::mem::size_of_val(inferred_span);
                traces.push(inferred_span.clone());
            }

            if let Some(ws) = &self.inferrer.wrapped_inferred_span {
                body_size += std::mem::size_of_val(ws);
                traces.push(ws.clone());
            }

            if let Some(cold_start_span) = &self.cold_start_span {
                body_size += std::mem::size_of_val(cold_start_span);
                traces.push(cold_start_span.clone());
            }

            // todo: figure out what to do here
            let header_tags = tracer_header_tags::TracerHeaderTags {
                lang: "",
                lang_version: "",
                lang_interpreter: "",
                lang_vendor: "",
                tracer_version: "",
                container_id: "",
                client_computed_top_level: false,
                client_computed_stats: false,
                dropped_p0_traces: 0,
                dropped_p0_spans: 0,
            };

            let send_data: SendData = trace_processor.process_traces(
                config.clone(),
                tags_provider.clone(),
                header_tags,
                vec![traces],
                body_size,
                self.inferrer.span_pointers.clone(),
                self.context_buffer.clone(),
            );

            if let Err(e) = trace_agent_tx.send(send_data).await {
                debug!("Failed to send invocation span to agent: {e}");
            }
        }
    }

    /// Given a `request_id` and the duration in milliseconds of the platform report,
    /// calculate the duration of the runtime if the `request_id` is found in the context buffer.
    ///
    /// If the `request_id` is not found in the context buffer, return `None`.
    /// If the `runtime_duration_ms` hasn't been seen, return `None`.
    ///
    pub fn on_platform_report(
        &mut self,
        request_id: &String,
        metrics: ReportMetrics,
        timestamp: i64,
    ) {
        // Set the report log metrics
        self.enhanced_metrics
            .set_report_log_metrics(&metrics, timestamp);

        let context_buffer = self.context_buffer.lock().expect("lock poisoned");
        if let Some(context) = context_buffer.get(request_id) {
            if context.runtime_duration_ms != 0.0 {
                let post_runtime_duration_ms = metrics.duration_ms - context.runtime_duration_ms;

                // Set the post runtime duration metric
                self.enhanced_metrics
                    .set_post_runtime_duration_metric(post_runtime_duration_ms, timestamp);
            }

            // Set Network and CPU time metrics
            if let Some(offsets) = context.enhanced_metric_data.clone() {
                self.enhanced_metrics
                    .set_network_enhanced_metrics(offsets.network_offset);
                self.enhanced_metrics
                    .set_cpu_time_enhanced_metrics(offsets.cpu_offset);
            }
        }
    }

    /// If this method is called, it means that we are operating in a Universally Instrumented
    /// runtime. Therefore, we need to set the `tracer_detected` flag to `true`.
    ///
    pub fn on_invocation_start(&mut self, headers: HashMap<String, String>, payload: Vec<u8>) {
        self.tracer_detected = true;

        let payload_value = serde_json::from_slice::<Value>(&payload).unwrap_or_else(|_| json!({}));

        // Tag the invocation span with the request payload
        if self.config.capture_lambda_payload {
            tag_span_from_value(
                &mut self.span,
                "function.request",
                &payload_value,
                0,
                self.config.capture_lambda_payload_max_depth,
            );
        }

        self.inferrer.infer_span(&payload_value, &self.aws_config);
        self.extracted_span_context = self.extract_span_context(&headers, &payload_value);

        // Set the extracted trace context to the spans
        if let Some(sc) = &self.extracted_span_context {
            self.span.trace_id = sc.trace_id;
            self.span.parent_id = sc.span_id;

            // Set the right data to the correct root level span,
            // If there's an inferred span, then that should be the root.
            if self.inferrer.inferred_span.is_some() {
                self.inferrer.set_parent_id(sc.span_id);
                self.inferrer.extend_meta(sc.tags.clone());
            } else {
                self.span.meta.extend(sc.tags.clone());
            }
        }

        // If we have an inferred span, set the invocation span parent id
        // to be the inferred span id, even if we don't have an extracted trace context
        if let Some(inferred_span) = &self.inferrer.inferred_span {
            self.span.parent_id = inferred_span.span_id;
        }
    }

    fn extract_span_context(
        &mut self,
        headers: &HashMap<String, String>,
        payload_value: &Value,
    ) -> Option<SpanContext> {
        if let Some(sc) = self.inferrer.get_span_context(&self.propagator) {
            return Some(sc);
        }

        if let Some(payload_headers) = payload_value.get("headers") {
            if let Some(sc) = self.propagator.extract(payload_headers) {
                debug!("Extracted trace context from event headers");
                return Some(sc);
            }
        }

        if let Some(sc) = self.propagator.extract(headers) {
            debug!("Extracted trace context from headers");
            return Some(sc);
        }

        None
    }

    /// Given trace context information, set it to the current span.
    ///
    pub fn on_invocation_end(&mut self, headers: HashMap<String, String>, payload: Vec<u8>) {
        let payload_value = serde_json::from_slice::<Value>(&payload).unwrap_or_else(|_| json!({}));

        // Tag the invocation span with the request payload
        if self.config.capture_lambda_payload {
            tag_span_from_value(
                &mut self.span,
                "function.response",
                &payload_value,
                0,
                self.config.capture_lambda_payload_max_depth,
            );
        }

        if let Some(status_code) = payload_value.get("statusCode").and_then(Value::as_i64) {
            let status_code_as_string = status_code.to_string();
            self.span.meta.insert(
                "http.status_code".to_string(),
                status_code_as_string.clone(),
            );

            if status_code_as_string.len() == 3 && status_code_as_string.starts_with('5') {
                self.span.error = 1;
            }

            // If we have an inferred span, set the status code to it
            self.inferrer.set_status_code(status_code_as_string);
        }

        self.update_span_context_from_headers(&headers);
        self.set_span_error_from_headers(headers);

        if self.span.error == 1 {
            let now = std::time::UNIX_EPOCH
                .elapsed()
                .expect("can't poll clock")
                .as_secs()
                .try_into()
                .unwrap_or_default();
            self.enhanced_metrics.increment_errors_metric(now);
        }
    }

    fn update_span_context_from_headers(&mut self, headers: &HashMap<String, String>) {
        let mut trace_id = 0;
        let mut parent_id = 0;
        let mut tags: HashMap<String, String> = HashMap::new();

        // If we have a trace context, this means we got it from
        // distributed tracing
        if let Some(sc) = &mut self.extracted_span_context {
            debug!("Trace context was found, not extracting it from incoming headers");
            trace_id = sc.trace_id;
            parent_id = sc.span_id;
            tags.extend(sc.tags.clone());
        }

        // We are the root span, so we should extract the trace context
        // from the tracer, which has sent it through end invocation headers
        if trace_id == 0 {
            debug!("No trace context found, extracting it from headers");
            // Extract trace context from headers manually
            if let Some(header) = headers.get(DATADOG_TRACE_ID_KEY) {
                trace_id = header.parse::<u64>().unwrap_or(0);
            }

            if let Some(header) = headers.get(DATADOG_PARENT_ID_KEY) {
                parent_id = header.parse::<u64>().unwrap_or(0);
            }

            if let Some(priority_str) = headers.get(DATADOG_SAMPLING_PRIORITY_KEY) {
                if let Ok(priority) = priority_str.parse::<f64>() {
                    self.span
                        .metrics
                        .insert(TAG_SAMPLING_PRIORITY.to_string(), priority);
                }
            }

            // Extract tags from headers
            // Used for 128 bit trace ids
            tags = DatadogHeaderPropagator::extract_tags(headers);
        }

        // We should always use the generated span id from the tracer
        if let Some(header) = headers.get(DATADOG_SPAN_ID_KEY) {
            self.span.span_id = header.parse::<u64>().unwrap_or(0);
        }

        self.span.trace_id = trace_id;

        if self.inferrer.inferred_span.is_some() {
            self.inferrer.extend_meta(tags);
        } else {
            self.span.parent_id = parent_id;
            self.span.meta.extend(tags);
        }
    }

    /// Given end invocation headers, set error metadata, if present, to the current span.
    ///
    fn set_span_error_from_headers(&mut self, headers: HashMap<String, String>) {
        let message = headers.get(DATADOG_INVOCATION_ERROR_MESSAGE_KEY);
        let r#type = headers.get(DATADOG_INVOCATION_ERROR_TYPE_KEY);
        let stack = headers.get(DATADOG_INVOCATION_ERROR_STACK_KEY);

        let is_error = headers
            .get(DATADOG_INVOCATION_ERROR_KEY)
            .map_or(false, |v| v.to_lowercase() == "true")
            || message.is_some()
            || stack.is_some()
            || r#type.is_some()
            || self.span.error == 1;
        if is_error {
            self.span.error = 1;

            if let Some(m) = message {
                let decoded_message = base64_to_string(m).unwrap_or_else(|_| {
                    debug!("Error message header may not be encoded, setting as is");
                    m.to_string()
                });

                self.span
                    .meta
                    .insert(String::from("error.msg"), decoded_message);
            }

            if let Some(t) = r#type {
                let decoded_type = base64_to_string(t).unwrap_or_else(|_| {
                    debug!("Error type header may not be encoded, setting as is");
                    t.to_string()
                });

                self.span
                    .meta
                    .insert(String::from("error.type"), decoded_type);
            }

            if let Some(s) = stack {
                let decoded_stack = base64_to_string(s).unwrap_or_else(|e| {
                    debug!("Failed to decode error stack: {e}");
                    s.to_string()
                });

                self.span
                    .meta
                    .insert(String::from("error.stack"), decoded_stack);
            }

            // todo: handle timeout
        }
    }

    pub fn on_out_of_memory_error(&mut self, timestamp: i64) {
        self.enhanced_metrics.increment_oom_metric(timestamp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LAMBDA_RUNTIME_SLUG;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use dogstatsd::aggregator::Aggregator;
    use dogstatsd::metric::EMPTY_TAGS;

    fn setup() -> Processor {
        let aws_config = AwsConfig {
            region: "us-east-1".into(),
            aws_access_key_id: "***".into(),
            aws_secret_access_key: "***".into(),
            aws_session_token: "***".into(),
            aws_container_credentials_full_uri: "***".into(),
            aws_container_authorization_token: "***".into(),
            function_name: "test-function".into(),
            sandbox_init_time: Instant::now(),
        };

        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: Some("test:tags".to_string()),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let metrics_aggregator = Arc::new(Mutex::new(
            Aggregator::new(EMPTY_TAGS, 1024).expect("failed to create aggregator"),
        ));

        Processor::new(
            tags_provider,
            config,
            &aws_config,
            metrics_aggregator,
            Arc::new(Mutex::new(ContextBuffer::default())),
        )
    }

    #[test]
    fn test_set_span_error_from_base64_encoded_headers() {
        let mut p = setup();
        let mut headers = HashMap::<String, String>::new();

        let error_message = "Error message";
        let error_type = "System.Exception";
        let error_stack =
            "System.Exception: Error message \n at TestFunction.Handle(ILambdaContext context)";

        headers.insert(DATADOG_INVOCATION_ERROR_KEY.into(), "true".into());
        headers.insert(
            DATADOG_INVOCATION_ERROR_MESSAGE_KEY.into(),
            STANDARD.encode(error_message),
        );
        headers.insert(
            DATADOG_INVOCATION_ERROR_TYPE_KEY.into(),
            STANDARD.encode(error_type),
        );
        headers.insert(
            DATADOG_INVOCATION_ERROR_STACK_KEY.into(),
            STANDARD.encode(error_stack),
        );

        p.set_span_error_from_headers(headers);

        assert_eq!(p.span.error, 1);
        assert_eq!(p.span.meta["error.msg"], error_message);
        assert_eq!(p.span.meta["error.type"], error_type);
        assert_eq!(p.span.meta["error.stack"], error_stack);
    }

    #[test]
    fn test_set_span_error_from_non_encoded_headers() {
        let mut p = setup();
        let mut headers = HashMap::<String, String>::new();

        let error_message = "Error message";
        let error_type = "System.Exception";
        let error_stack =
            "System.Exception: Error message \n at TestFunction.Handle(ILambdaContext context)";

        headers.insert(DATADOG_INVOCATION_ERROR_KEY.into(), "true".into());
        headers.insert(
            DATADOG_INVOCATION_ERROR_MESSAGE_KEY.into(),
            error_message.into(),
        );
        headers.insert(DATADOG_INVOCATION_ERROR_TYPE_KEY.into(), error_type.into());
        headers.insert(
            DATADOG_INVOCATION_ERROR_STACK_KEY.into(),
            error_stack.into(),
        );

        p.set_span_error_from_headers(headers);

        assert_eq!(p.span.error, 1);
        assert_eq!(p.span.meta["error.msg"], error_message);
        assert_eq!(p.span.meta["error.type"], error_type);
        assert_eq!(p.span.meta["error.stack"], error_stack);
    }

    #[test]
    fn test_update_span_context_with_sampling_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "999".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "1000".to_string());
        headers.insert(DATADOG_SAMPLING_PRIORITY_KEY.to_string(), "-1".to_string());

        p.update_span_context_from_headers(&headers);

        assert_eq!(p.span.trace_id, 999);
        assert_eq!(p.span.parent_id, 1000);
        let priority = p.span.metrics.get(TAG_SAMPLING_PRIORITY).copied();
        assert_eq!(priority, Some(-1.0));
    }

    #[test]
    fn test_update_span_context_with_invalid_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "888".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "999".to_string());
        headers.insert(
            DATADOG_SAMPLING_PRIORITY_KEY.to_string(),
            "not-a-number".to_string(),
        );

        p.update_span_context_from_headers(&headers);

        assert!(!p.span.metrics.contains_key(TAG_SAMPLING_PRIORITY));
        assert_eq!(p.span.trace_id, 888);
        assert_eq!(p.span.parent_id, 999);
    }

    #[test]
    fn test_update_span_context_no_sampling_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "111".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "222".to_string());

        p.update_span_context_from_headers(&headers);

        assert!(!p.span.metrics.contains_key(TAG_SAMPLING_PRIORITY));
        assert_eq!(p.span.trace_id, 111);
        assert_eq!(p.span.parent_id, 222);
    }

    #[test]
    fn parsing_status_code() {
        let mut p = setup();

        let response = r#"
       {
           "statusCode": 200,
           "headers": {
               "Content-Type": "application/json"
           },
           "isBase64Encoded": false,
           "multiValueHeaders": {
               "X-Custom-Header": ["My value", "My other value"]
           },
           "body": "{\n  \"TotalCodeSize\": 104330022,\n  \"FunctionCount\": 26\n}"
       }
       "#;

        p.on_invocation_end(HashMap::new(), response.as_bytes().to_vec());

        assert_eq!(
            p.span
                .meta
                .get("http.status_code")
                .expect("Status code not parsed!"),
            "200"
        );
    }
}
