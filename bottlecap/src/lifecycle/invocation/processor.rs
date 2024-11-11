use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use datadog_trace_protobuf::pb::Span;
use datadog_trace_utils::{send_data::SendData, tracer_header_tags};
use dogstatsd::aggregator::Aggregator as MetricsAggregator;
use serde_json::{json, Value};
use tokio::sync::mpsc::Sender;
use tracing::debug;

use crate::{
    config::{self, AwsConfig},
    lifecycle::invocation::{
        base64_to_string, context::ContextBuffer, span_inferrer::SpanInferrer,
    },
    metrics::enhanced::lambda::{EnhancedMetricData, Lambda as EnhancedMetrics},
    proc::{self, CPUData, NetworkData},
    tags::provider,
    telemetry::events::{ReportMetrics, Status},
    traces::{
        context::SpanContext,
        propagation::{DatadogCompositePropagator, Propagator},
        trace_processor,
    },
};

pub const MS_TO_NS: f64 = 1_000_000.0;
pub const S_TO_NS: f64 = 1_000_000_000.0;

pub const DATADOG_INVOCATION_ERROR_MESSAGE_KEY: &str = "x-datadog-invocation-error-msg";
pub const DATADOG_INVOCATION_ERROR_TYPE_KEY: &str = "x-datadog-invocation-error-type";
pub const DATADOG_INVOCATION_ERROR_STACK_KEY: &str = "x-datadog-invocation-error-stack";
pub const DATADOG_INVOCATION_ERROR_KEY: &str = "x-datadog-invocation-error";

pub struct Processor {
    pub context_buffer: ContextBuffer,
    inferrer: SpanInferrer,
    pub span: Span,
    pub extracted_span_context: Option<SpanContext>,
    // Used to extract the trace context from inferred span, headers, or payload
    propagator: DatadogCompositePropagator,
    enhanced_metrics: EnhancedMetrics,
    aws_config: AwsConfig,
    tracer_detected: bool,
    enhanced_metrics_enabled: bool,
}

impl Processor {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
        aws_config: &AwsConfig,
        metrics_aggregator: Arc<Mutex<MetricsAggregator>>,
    ) -> Self {
        let service = config.service.clone().unwrap_or("aws.lambda".to_string());
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or("aws_lambda".to_string());

        let propagator = DatadogCompositePropagator::new(Arc::clone(&config));

        Processor {
            context_buffer: ContextBuffer::default(),
            inferrer: SpanInferrer::default(),
            span: Span {
                service,
                name: "aws.lambda".to_string(),
                resource,
                trace_id: 0,
                span_id: 0,
                parent_id: 0,
                start: 0,
                duration: 0,
                error: 0,
                meta: HashMap::new(),
                metrics: HashMap::new(),
                r#type: "serverless".to_string(),
                meta_struct: HashMap::new(),
                span_links: Vec::new(),
            },
            extracted_span_context: None,
            propagator,
            enhanced_metrics: EnhancedMetrics::new(metrics_aggregator, Arc::clone(&config)),
            aws_config: aws_config.clone(),
            tracer_detected: false,
            enhanced_metrics_enabled: config.enhanced_metrics,
        }
    }

    /// Given a `request_id`, creates the context and adds the enhanced metric offsets to the context buffer.
    ///
    pub fn on_invoke_event(&mut self, request_id: String) {
        self.context_buffer.create_context(request_id.clone());
        if self.enhanced_metrics_enabled {
            let network_offset: Option<NetworkData> = proc::get_network_data().ok();
            let cpu_offset: Option<CPUData> = proc::get_cpu_data().ok();
            let uptime_offset: Option<f64> = proc::get_uptime().ok();
            let enhanced_metric_offsets = Some(EnhancedMetricData {
                network_offset,
                cpu_offset,
                uptime_offset,
            });
            self.context_buffer
                .add_enhanced_metric_data(&request_id, enhanced_metric_offsets);
        }

        // Increment the invocation metric
        self.enhanced_metrics.increment_invocation_metric();
    }

    /// Given the duration of the platform init report, set the init duration metric.
    ///
    pub fn on_platform_init_report(&mut self, duration_ms: f64) {
        self.enhanced_metrics.set_init_duration_metric(duration_ms);
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
        self.context_buffer.add_start_time(&request_id, start_time);
        self.span.start = start_time;
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::cast_possible_truncation)]
    pub async fn on_platform_runtime_done(
        &mut self,
        request_id: &String,
        duration_ms: f64,
        status: Status,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_agent_tx: Sender<SendData>,
    ) {
        self.context_buffer
            .add_runtime_duration(request_id, duration_ms);

        // Set the runtime duration metric
        self.enhanced_metrics
            .set_runtime_duration_metric(duration_ms);

        if status != Status::Success {
            // Increment the error metric
            self.enhanced_metrics.increment_errors_metric();

            // Increment the error type metric
            if status == Status::Timeout {
                self.enhanced_metrics.increment_timeout_metric();
            }
        }

        if let Some(context) = self.context_buffer.get(request_id) {
            let span = &mut self.span;
            // `round` is intentionally meant to be a whole integer
            span.duration = (context.runtime_duration_ms * MS_TO_NS).round() as i64;
            span.meta
                .insert("request_id".to_string(), request_id.clone());
            // todo(duncanista): add missing tags
            // - cold start, proactive init
            // - language
            // - function.request - capture lambda payload
            // - function.response
            // - error.msg
            // - error.type
            // - error.stack
            // - metrics tags (for asm)

            if let Some(offsets) = &context.enhanced_metric_data {
                self.enhanced_metrics.set_cpu_utilization_enhanced_metrics(
                    offsets.cpu_offset.clone(),
                    offsets.uptime_offset,
                );
            }
        }

        if let Some(trigger_tags) = self.inferrer.get_trigger_tags() {
            self.span.meta.extend(trigger_tags);
        }

        self.inferrer.complete_inferred_spans(&self.span);

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
            };

            let send_data = trace_processor.process_traces(
                config.clone(),
                tags_provider.clone(),
                header_tags,
                vec![traces],
                body_size,
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
    pub fn on_platform_report(&mut self, request_id: &String, metrics: ReportMetrics) {
        // Set the report log metrics
        self.enhanced_metrics.set_report_log_metrics(&metrics);

        if let Some(context) = self.context_buffer.remove(request_id) {
            if context.runtime_duration_ms != 0.0 {
                let post_runtime_duration_ms = metrics.duration_ms - context.runtime_duration_ms;

                // Set the post runtime duration metric
                self.enhanced_metrics
                    .set_post_runtime_duration_metric(post_runtime_duration_ms);
            }

            // Set Network and CPU time metrics
            if let Some(offsets) = context.enhanced_metric_data {
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

        // Reset trace context
        self.span.trace_id = 0;
        self.span.parent_id = 0;
        self.span.span_id = 0;
        self.extracted_span_context = None;

        let payload_value = match serde_json::from_slice::<Value>(&payload) {
            Ok(value) => value,
            Err(_) => json!({}),
        };

        self.inferrer.infer_span(&payload_value, &self.aws_config);
        self.extracted_span_context = self.extract_span_context(&headers, &payload_value);

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

        if let Some(inferred_span) = &self.inferrer.inferred_span {
            self.span.parent_id = inferred_span.span_id;
        }
    }

    fn extract_span_context(
        &mut self,
        headers: &HashMap<String, String>,
        payload_value: &Value,
    ) -> Option<SpanContext> {
        if let Some(carrier) = self.inferrer.get_carrier() {
            if let Some(sc) = self.propagator.extract(&carrier) {
                debug!("Extracted trace context from inferred span");
                return Some(sc);
            }
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
    pub fn on_invocation_end(
        &mut self,
        headers: HashMap<String, String>,
        status_code: Option<String>,
    ) {
        if let Some(status_code) = status_code {
            self.span
                .meta
                .insert("http.status_code".to_string(), status_code.clone());

            if status_code.len() == 3 && status_code.starts_with('5') {
                self.span.error = 1;
            }

            // If we have an inferred span, set the status code to it
            self.inferrer.set_status_code(status_code);
        }

        self.update_span_context_from_headers(&headers);
        self.set_span_error_from_headers(headers);

        if self.span.error == 1 {
            self.enhanced_metrics.increment_errors_metric();
        }
    }

    fn update_span_context_from_headers(&mut self, headers: &HashMap<String, String>) {
        // todo: fix this, code is a copy of the existing logic in Go, not accounting
        // when a 128 bit trace id exist
        let mut trace_id = 0;
        let mut span_id = 0;
        let mut parent_id = 0;

        // If we have a trace context, update the span context
        if let Some(sc) = &mut self.extracted_span_context {
            trace_id = sc.trace_id;
            span_id = sc.span_id;
        }

        if let Some(header) = headers.get("x-datadog-trace-id") {
            trace_id = header.parse::<u64>().unwrap_or(0);
        }

        if let Some(header) = headers.get("x-datadog-span-id") {
            span_id = header.parse::<u64>().unwrap_or(0);
        }

        if let Some(header) = headers.get("x-datadog-parent-id") {
            parent_id = header.parse::<u64>().unwrap_or(0);
        }

        self.span.trace_id = trace_id;
        self.span.span_id = span_id;

        // If no inferred span, set the parent id right away
        if self.inferrer.inferred_span.is_none() {
            self.span.parent_id = parent_id;
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
                self.span
                    .meta
                    .insert(String::from("error.msg"), m.to_string());
            }

            if let Some(t) = r#type {
                self.span
                    .meta
                    .insert(String::from("error.type"), t.to_string());
            }

            if let Some(s) = stack {
                let decoded_stack = match base64_to_string(s) {
                    Ok(decoded) => decoded,
                    Err(e) => {
                        debug!("Failed to decode error stack: {e}");
                        s.to_string()
                    }
                };

                self.span
                    .meta
                    .insert(String::from("error.stack"), decoded_stack);
            }

            // todo: handle timeout
        }
    }
}
