use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use libdd_trace_protobuf::pb::Span;
use libdd_trace_utils::tracer_header_tags;
use serde_json::Value;
use tokio::time::Instant;
use tracing::{debug, warn};

use crate::{
    config::{self, aws::AwsConfig},
    extension::telemetry::events::{
        InitType, ManagedInstanceReportMetrics, OnDemandReportMetrics, ReportMetrics,
        RuntimeDoneMetrics, Status,
    },
    lifecycle::invocation::{
        base64_to_string,
        context::{Context, ContextBuffer, ReparentingInfo},
        create_empty_span, generate_span_id, get_metadata_from_value,
        span_inferrer::{self, SpanInferrer},
        triggers::get_default_service_name,
    },
    metrics::enhanced::lambda::{EnhancedMetricData, Lambda as EnhancedMetrics},
    proc::{
        self, CPUData, NetworkData,
        constants::{ETC_PATH, PROC_PATH},
    },
    tags::{lambda::tags::resolve_runtime_from_proc, provider},
    traces::{
        context::SpanContext,
        propagation::{
            DatadogCompositePropagator, Propagator,
            text_map_propagator::{
                DATADOG_PARENT_ID_KEY, DATADOG_SAMPLING_PRIORITY_KEY, DATADOG_SPAN_ID_KEY,
                DATADOG_TRACE_ID_KEY, DatadogHeaderPropagator,
            },
        },
        trace_processor::SendingTraceProcessor,
    },
};

pub const MS_TO_NS: f64 = 1_000_000.0;
pub const S_TO_MS: u64 = 1_000;
pub const S_TO_NS: f64 = 1_000_000_000.0;
pub const PROACTIVE_INITIALIZATION_THRESHOLD_MS: u64 = 10_000;

pub const DATADOG_INVOCATION_ERROR_MESSAGE_KEY: &str = "x-datadog-invocation-error-msg";
pub const DATADOG_INVOCATION_ERROR_TYPE_KEY: &str = "x-datadog-invocation-error-type";
pub const DATADOG_INVOCATION_ERROR_STACK_KEY: &str = "x-datadog-invocation-error-stack";
pub const DATADOG_INVOCATION_ERROR_KEY: &str = "x-datadog-invocation-error";
const TAG_SAMPLING_PRIORITY: &str = "_sampling_priority_v1";

pub struct Processor {
    /// Buffer containing context of the previous 5 invocations
    context_buffer: ContextBuffer,
    /// Helper class to infer upstream span information and
    /// extract trace context if available.
    inferrer: SpanInferrer,
    /// Propagator to extract span context from carriers.
    propagator: Arc<DatadogCompositePropagator>,
    /// Helper class to send enhanced metrics.
    enhanced_metrics: EnhancedMetrics,
    /// AWS configuration from the Lambda environment.
    aws_config: Arc<AwsConfig>,
    /// Flag to determine if a tracer was detected through
    /// universal instrumentation.
    tracer_detected: bool,
    /// Runtime of the Lambda function.
    ///
    /// This is set on the first invocation and reused for every
    /// other invocation. Since we have to resolve the runtime
    /// from the proc filesystem, it's not possible to know the
    /// runtime before the first invocation.
    runtime: Option<String>,
    config: Arc<config::Config>,
    service: String,
    resource: String,
    /// Dynamic tags calculated during the start of the invocation.
    ///
    /// These tags are used to capture runtime and initialization.
    dynamic_tags: HashMap<String, String>,
    /// Tracks active concurrent invocations for monitoring enhanced metrics in Managed Instance mode.
    active_invocations: usize,
    /// Tracks whether if first invocation after init has been received in Managed Instance mode.
    /// Used to determine if we should search for the empty context on an invocation.
    awaiting_first_invocation: bool,
}

impl Processor {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
        aws_config: Arc<AwsConfig>,
        metrics_aggregator: dogstatsd::aggregator_service::AggregatorHandle,
        propagator: Arc<DatadogCompositePropagator>,
    ) -> Self {
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or(String::from("aws.lambda"));

        let service = get_default_service_name(
            &config.service.clone().unwrap_or(resource.clone()),
            "aws.lambda",
            config.trace_aws_service_representation_enabled,
        )
        .to_lowercase();

        let enhanced_metrics = EnhancedMetrics::new(metrics_aggregator, Arc::clone(&config));
        enhanced_metrics.start_usage_metrics_task(); // starts the long-running task that monitors usage metrics (fd_use, threads_use, tmp_used)

        Processor {
            context_buffer: ContextBuffer::default(),
            inferrer: SpanInferrer::new(Arc::clone(&config)),
            propagator,
            enhanced_metrics,
            aws_config,
            tracer_detected: false,
            runtime: None,
            config: Arc::clone(&config),
            service,
            resource,
            dynamic_tags: HashMap::new(),
            active_invocations: 0,
            awaiting_first_invocation: false,
        }
    }

    /// Given a `request_id`, creates the context and adds the enhanced metric offsets to the context buffer.
    ///
    pub fn on_invoke_event(&mut self, request_id: String) {
        // In Managed Instance mode, if awaiting the first invocation after init, find and update the empty context created on init start
        if self.aws_config.is_managed_instance_mode() && self.awaiting_first_invocation {
            if self
                .context_buffer
                .update_empty_context_request_id(&request_id)
            {
                debug!(
                    "Updated empty context from init start with request_id: {}",
                    request_id
                );
            } else {
                debug!("Expected empty context but not found, creating new context");
                let invocation_span =
                    create_empty_span(String::from("aws.lambda"), &self.resource, &self.service);
                self.set_init_tags();
                self.context_buffer
                    .start_context(&request_id, invocation_span);
            }
            self.awaiting_first_invocation = false;
        } else {
            let invocation_span =
                create_empty_span(String::from("aws.lambda"), &self.resource, &self.service);
            // Important! Call set_init_tags() before adding the invocation to the context buffer
            self.set_init_tags();
            self.context_buffer
                .start_context(&request_id, invocation_span);
        }

        let timestamp = std::time::UNIX_EPOCH
            .elapsed()
            .expect("can't poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();

        if self.config.lambda_proc_enhanced_metrics {
            if self.aws_config.is_managed_instance_mode() {
                // In Managed Instance mode, track concurrent invocations
                self.active_invocations += 1;

                // Start monitoring on the first active invocation
                if self.active_invocations == 1 {
                    debug!("Starting usage metrics monitoring");
                    self.enhanced_metrics.resume_usage_metrics_monitoring();
                }
            } else {
                // In On-Demand mode, we reset metrics and resume monitoring on each invocation
                self.enhanced_metrics.restart_usage_metrics_monitoring();
            }

            // Collect offsets for network and cpu metrics
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
        self.enhanced_metrics.increment_invocation_metric(timestamp);
        self.enhanced_metrics.set_invoked_received();

        // MANAGED INSTANCE MODE: Check for buffered UniversalInstrumentationStart with request_id
        if self.aws_config.is_managed_instance_mode() {
            if let Some(buffered_event) = self
                .context_buffer
                .take_universal_instrumentation_start_for_request_id(&request_id)
            {
                debug!(
                    "Managed Instance: Found buffered UniversalInstrumentationStart for request_id: {}",
                    request_id
                );
                // Infer span
                self.inferrer
                    .infer_span(&buffered_event.payload_value, &self.aws_config);
                self.process_on_universal_instrumentation_start(
                    request_id,
                    buffered_event.headers,
                    buffered_event.payload_value,
                );
            }
            return;
        }

        // ON-DEMAND MODE: Use existing FIFO pairing logic (unchanged)
        // If `UniversalInstrumentationStart` event happened first, process it
        if let Some((headers, payload_value)) = self.context_buffer.pair_invoke_event(&request_id) {
            // Infer span
            self.inferrer.infer_span(&payload_value, &self.aws_config);
            self.process_on_universal_instrumentation_start(request_id, headers, payload_value);
        }
    }

    /// On the first invocation, determine if it's a cold start or proactive init.
    ///
    /// For every other invocation, it will always be warm start.
    ///
    fn set_init_tags(&mut self) {
        let mut proactive_initialization = false;
        let mut cold_start = false;

        // If it's empty, then we are in a cold start
        if self.context_buffer.is_empty() {
            let now = Instant::now();
            let time_since_sandbox_init = now.duration_since(self.aws_config.sandbox_init_time);
            if time_since_sandbox_init.as_millis() > PROACTIVE_INITIALIZATION_THRESHOLD_MS.into() {
                proactive_initialization = true;
            } else {
                cold_start = true;
            }

            // Resolve runtime only once
            let runtime = resolve_runtime_from_proc(PROC_PATH, ETC_PATH);
            self.runtime = Some(runtime);
        }

        // Skip cold_start tag in Managed Instance mode
        // We don't want to send this tag for spans, as the experience
        // won't look good due to the time gap in the flame graph.
        if !self.aws_config.is_managed_instance_mode() {
            self.dynamic_tags
                .insert(String::from("cold_start"), cold_start.to_string());
        }

        if proactive_initialization {
            self.dynamic_tags.insert(
                String::from("proactive_initialization"),
                proactive_initialization.to_string(),
            );
        }

        if let Some(runtime) = &self.runtime {
            self.dynamic_tags
                .insert(String::from("runtime"), runtime.clone());
            self.enhanced_metrics.set_runtime_tag(runtime);
        }

        self.enhanced_metrics
            .set_init_tags(proactive_initialization, cold_start);
    }

    /// Called when the platform init starts.
    ///
    /// This is used to create a cold start span, since this telemetry event does not
    /// provide a `request_id`, we try to guess which invocation is the cold start.
    pub fn on_platform_init_start(&mut self, time: DateTime<Utc>, runtime_version: Option<String>) {
        if runtime_version
            .as_deref()
            .is_some_and(|rv| rv.contains("DurableFunction"))
        {
            self.enhanced_metrics.set_durable_function_tag();
        }
        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();

        // In Managed Instance mode, create a context with empty request_id which will be updated on the first platform start received.
        // In On-Demand mode, InitStart is received after the Invoke event, so we get the closest context by timestamp as it does not have a request_id.
        let context = if self.aws_config.is_managed_instance_mode() {
            let invocation_span =
                create_empty_span(String::from("aws.lambda"), &self.resource, &self.service);
            self.set_init_tags();
            self.context_buffer.start_context("", invocation_span);
            self.awaiting_first_invocation = true;

            self.context_buffer.get_mut(&String::new())
        } else {
            self.context_buffer.get_closest_mut(start_time)
        };

        let Some(context) = context else {
            debug!("Cannot process on platform init start, no invocation context found");
            return;
        };

        // Skip creating cold start span in Managed Instances
        // Although the telemetry is correct, we instead decide
        // to not send it since the flame graph would show a big
        // gap between the init and the first invocation.
        if self.aws_config.is_managed_instance_mode() {
            return;
        }

        // Create a cold start span
        let mut cold_start_span = create_empty_span(
            String::from("aws.lambda.cold_start"),
            &self.resource,
            &self.service,
        );
        cold_start_span.span_id = generate_span_id();
        cold_start_span.start = start_time;
        context.cold_start_span = Some(cold_start_span);
    }

    /// Given the duration of the platform init report, set the init duration metric.
    ///
    #[allow(clippy::cast_possible_truncation)]
    pub fn on_platform_init_report(
        &mut self,
        init_type: InitType,
        duration_ms: f64,
        timestamp: i64,
    ) {
        self.enhanced_metrics
            .set_init_duration_metric(init_type, duration_ms, timestamp);

        // In Managed Instance mode, find the context with empty request_id
        // In On-Demand mode, find the closest context by timestamp since we do not have the request_id
        let context = if self.aws_config.is_managed_instance_mode() {
            self.context_buffer.get_mut(&String::new())
        } else {
            self.context_buffer.get_closest_mut(timestamp)
        };

        let Some(context) = context else {
            debug!("Cannot process on platform init report, no invocation context found");
            return;
        };

        // Add duration to cold start span
        if let Some(cold_start_span) = &mut context.cold_start_span {
            // `round` is intentionally meant to be a whole integer
            cold_start_span.duration = (duration_ms * MS_TO_NS) as i64;
        }
    }

    /// Called when the `SnapStart` restore phase starts.
    ///
    /// This is used to create a `snapstart_restore` span, since this telemetry event does not
    /// provide a `request_id`, we try to guess which invocation is the restore similar to init.
    pub fn on_platform_restore_start(&mut self, time: DateTime<Utc>) {
        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();

        // Get the closest context
        let Some(context) = self.context_buffer.get_closest_mut(start_time) else {
            debug!("Cannot process on platform restore start, no invocation context found");
            return;
        };

        // Create a SnapStart restore span
        let mut snapstart_restore_span = create_empty_span(
            String::from("aws.lambda.snapstart_restore"),
            &self.resource,
            &self.service,
        );
        snapstart_restore_span.span_id = generate_span_id();
        snapstart_restore_span.start = start_time;
        context.snapstart_restore_span = Some(snapstart_restore_span);
    }

    /// Given the duration of the platform restore report, set the snapstart restore duration.
    ///
    #[allow(clippy::cast_possible_truncation)]
    pub fn on_platform_restore_report(&mut self, duration_ms: f64, timestamp: i64) {
        self.enhanced_metrics
            .set_snapstart_restore_duration_metric(duration_ms, timestamp);

        let Some(context) = self.context_buffer.get_closest_mut(timestamp) else {
            debug!("Cannot process on platform restore report, no invocation context found");
            return;
        };

        if let Some(snapstart_restore_span) = &mut context.snapstart_restore_span {
            // `round` is intentionally meant to be a whole integer
            snapstart_restore_span.duration = (duration_ms * MS_TO_NS) as i64;
        }
    }

    /// Given a `request_id` and the time of the platform start, add the start time to the context buffer.
    ///
    pub fn on_platform_start(&mut self, request_id: String, time: DateTime<Utc>) {
        if self.aws_config.is_managed_instance_mode() {
            self.on_invoke_event(request_id.clone());
        }

        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();
        self.context_buffer.add_start_time(&request_id, start_time);
    }

    #[must_use]
    pub fn is_managed_instance_mode(&self) -> bool {
        self.aws_config.is_managed_instance_mode()
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::cast_possible_truncation)]
    pub async fn on_platform_runtime_done(
        &mut self,
        request_id: &String,
        metrics: RuntimeDoneMetrics,
        status: Status,
        error_type: Option<String>,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
        timestamp: i64,
    ) {
        // Set the runtime duration metric
        self.enhanced_metrics
            .set_runtime_done_metrics(&metrics, timestamp);

        if status != Status::Success {
            // Increment the error metric
            self.enhanced_metrics.increment_errors_metric(timestamp);

            // Increment the error type metric
            if status == Status::Timeout {
                self.enhanced_metrics.increment_timeout_metric(timestamp);
            }

            if status == Status::Error && error_type == Some("Runtime.OutOfMemory".to_string()) {
                debug!(
                    "Invocation Processor | PlatformRuntimeDone | Got Runtime.OutOfMemory. Incrementing OOM metric."
                );
                self.enhanced_metrics.increment_oom_metric(timestamp);
            }
        }

        // In On-Demand mode, pause monitoring between invocations and emit the metrics on each invocation
        if !self.aws_config.is_managed_instance_mode() {
            self.enhanced_metrics.set_max_enhanced_metrics();
            self.enhanced_metrics.set_usage_enhanced_metrics();
        }

        self.context_buffer
            .add_runtime_duration(request_id, metrics.duration_ms);

        // MANAGED INSTANCE MODE: Check for buffered UniversalInstrumentationEnd with request_id
        if self.aws_config.is_managed_instance_mode() {
            if let Some(buffered_event) = self
                .context_buffer
                .take_universal_instrumentation_end_for_request_id(request_id)
            {
                debug!(
                    "Managed Instance: Found buffered UniversalInstrumentationEnd for request_id: {}",
                    request_id
                );
                self.process_on_universal_instrumentation_end(
                    request_id.clone(),
                    buffered_event.headers,
                    buffered_event.payload_value,
                );
            }
        } else {
            // ON-DEMAND MODE: Use existing FIFO pairing logic (unchanged)
            // If `UniversalInstrumentationEnd` event happened first, process it first
            if let Some((headers, payload)) = self
                .context_buffer
                .pair_platform_runtime_done_event(request_id)
            {
                self.process_on_universal_instrumentation_end(request_id.clone(), headers, payload);
            }
        }

        self.process_on_platform_runtime_done(request_id, status, tags_provider, trace_sender)
            .await;
    }

    async fn process_on_platform_runtime_done(
        &mut self,
        request_id: &String,
        status: Status,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
    ) {
        let context = self.enrich_ctx_at_platform_done(request_id, status);

        if self.tracer_detected {
            if let Some(ctx) = context
                && ctx.invocation_span.trace_id != 0
                && ctx.invocation_span.span_id != 0
            {
                self.send_ctx_spans(&tags_provider, &trace_sender, ctx)
                    .await;
            }
        } else {
            self.send_cold_start_span(&tags_provider, &trace_sender)
                .await;
        }
    }

    fn enrich_ctx_at_platform_done(
        &mut self,
        request_id: &String,
        status: Status,
    ) -> Option<Context> {
        let Some(context) = self.context_buffer.get_mut(request_id) else {
            debug!(
                "Cannot process on platform runtime done, no invocation context found for request_id: {request_id}"
            );
            return None;
        };
        context.runtime_done_received = true;

        // Handle timeout error case
        if status == Status::Timeout {
            if context.invocation_span.trace_id == 0 {
                context.invocation_span.trace_id = generate_span_id();
            }
            if context.invocation_span.span_id == 0 {
                context.invocation_span.span_id = generate_span_id();
            }
            context.invocation_span.error = 1; // Mark as error
            context.invocation_span.meta.insert(
                "error.msg".to_string(),
                "Datadog detected a Timeout".to_string(),
            );
            context
                .invocation_span
                .meta
                .insert("error.type".to_string(), "Timeout".to_string());
        }

        // Process enhanced metrics if available
        if let Some(offsets) = &context.enhanced_metric_data {
            self.enhanced_metrics.set_cpu_utilization_enhanced_metrics(
                offsets.cpu_offset.clone(),
                offsets.uptime_offset,
            );
        }

        // todo(duncanista): Add missing metric tags for ASM
        // Add dynamic and trigger tags
        context
            .invocation_span
            .meta
            .extend(self.dynamic_tags.clone());

        if let Some(trigger_tags) = self.inferrer.get_trigger_tags() {
            context.invocation_span.meta.extend(trigger_tags);
        }

        self.inferrer
            .complete_inferred_spans(&context.invocation_span);

        // Handle cold start span if present
        if let Some(cold_start_span) = &mut context.cold_start_span
            && context.invocation_span.trace_id != 0
        {
            cold_start_span.trace_id = context.invocation_span.trace_id;
            cold_start_span.parent_id = context.invocation_span.parent_id;
        }

        // Handle snapstart restore span if present
        if let Some(snapstart_restore_span) = &mut context.snapstart_restore_span
            && context.invocation_span.trace_id != 0
        {
            snapstart_restore_span.trace_id = context.invocation_span.trace_id;
            snapstart_restore_span.parent_id = context.invocation_span.parent_id;
        }
        Some(context.clone())
    }

    pub async fn send_ctx_spans(
        &mut self,
        tags_provider: &Arc<provider::Provider>,
        trace_sender: &Arc<SendingTraceProcessor>,
        context: Context,
    ) {
        let (traces, body_size) = self.get_ctx_spans(context);
        self.send_spans(traces, body_size, tags_provider, trace_sender)
            .await;
    }

    fn get_ctx_spans(&mut self, context: Context) -> (Vec<Span>, usize) {
        let mut body_size = std::mem::size_of_val(&context.invocation_span);
        let mut traces = vec![context.invocation_span.clone()];

        if let Some(inferred_span) = &self.inferrer.inferred_span {
            body_size += std::mem::size_of_val(inferred_span);
            traces.push(inferred_span.clone());
        }

        if let Some(ws) = &self.inferrer.wrapped_inferred_span {
            body_size += std::mem::size_of_val(ws);
            traces.push(ws.clone());
        }

        // SnapStart includes telemetry events from Init (Cold Start).
        // However, these Init events are from when the snapshot was created and
        // not when the lambda sandbox is actually created.
        // So, if we have a snapstart restore span, use it instead of cold start span.
        if let Some(snapstart_restore_span) = &context.snapstart_restore_span {
            body_size += std::mem::size_of_val(snapstart_restore_span);
            traces.push(snapstart_restore_span.clone());
        } else if let Some(cold_start_span) = &context.cold_start_span {
            body_size += std::mem::size_of_val(cold_start_span);
            traces.push(cold_start_span.clone());
        }

        (traces, body_size)
    }

    /// For Node/Python: Updates the cold start span with the given trace ID.
    /// Returns the Span ID of the cold start span so we can reparent the `aws.lambda.load` span.
    pub fn set_cold_start_span_trace_id(&mut self, trace_id: u64) -> Option<u64> {
        if let Some(cold_start_context) = self.context_buffer.get_context_with_cold_start()
            && let Some(cold_start_span) = &mut cold_start_context.cold_start_span
        {
            if cold_start_span.trace_id == 0 {
                cold_start_span.trace_id = trace_id;
            }

            return Some(cold_start_span.span_id);
        }

        None
    }

    /// For Node/Python: Sends the cold start span to the trace agent.
    async fn send_cold_start_span(
        &mut self,
        tags_provider: &Arc<provider::Provider>,
        trace_sender: &Arc<SendingTraceProcessor>,
    ) {
        if let Some(cold_start_context) = self.context_buffer.get_context_with_cold_start()
            && let Some(cold_start_span) = &mut cold_start_context.cold_start_span
        {
            if cold_start_span.trace_id == 0 {
                debug!("Not sending cold start span because trace ID is unset.");
                return;
            }

            let traces = vec![cold_start_span.clone()];
            let body_size = size_of_val(cold_start_span);

            self.send_spans(traces, body_size, tags_provider, trace_sender)
                .await;
        }
    }

    /// Used by universally instrumented runtimes to send context spans:
    /// invocation span, inferred span(s), & cold start span.
    /// Used by Node+Python to send cold start span.
    async fn send_spans(
        &mut self,
        traces: Vec<Span>,
        body_size: usize,
        tags_provider: &Arc<provider::Provider>,
        trace_sender: &Arc<SendingTraceProcessor>,
    ) {
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

        if let Err(e) = trace_sender
            .send_processed_traces(
                self.config.clone(),
                tags_provider.clone(),
                header_tags,
                vec![traces],
                body_size,
                self.inferrer.span_pointers.clone(),
            )
            .await
        {
            debug!("Failed to send context spans to agent: {e}");
        }
    }

    /// Given a `request_id` and the duration in milliseconds of the platform report,
    /// calculate the duration of the runtime if the `request_id` is found in the context buffer.
    ///
    /// If the `request_id` is not found in the context buffer, return `None`.
    /// If the `runtime_duration_ms` hasn't been seen, return `None`.
    ///
    #[allow(clippy::too_many_arguments)]
    pub async fn on_platform_report(
        &mut self,
        request_id: &String,
        metrics: ReportMetrics,
        timestamp: i64,
        status: Status,
        error_type: Option<String>,
        spans: Option<Vec<crate::extension::telemetry::events::TelemetrySpan>>,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
    ) {
        // Set the report log metrics
        self.enhanced_metrics
            .set_report_log_metrics(&metrics, timestamp);

        match metrics {
            ReportMetrics::ManagedInstance(metric) => {
                self.handle_managed_instance_report(
                    request_id,
                    metric,
                    timestamp,
                    status,
                    error_type,
                    spans,
                    tags_provider,
                    trace_sender,
                )
                .await;
            }
            ReportMetrics::OnDemand(metrics) => {
                self.handle_ondemand_report(request_id, metrics, timestamp);
            }
        }

        // Set Network and CPU time metrics
        if let Some(context) = self.context_buffer.get(request_id)
            && let Some(offsets) = &context.enhanced_metric_data
        {
            self.enhanced_metrics
                .set_network_enhanced_metrics(offsets.network_offset);
            self.enhanced_metrics
                .set_cpu_time_enhanced_metrics(offsets.cpu_offset.clone());
        }
    }

    /// Handles Managed Instance mode platform report processing.
    ///
    /// In Managed Instance mode, platform.runtimeDone events are not sent, so this function
    /// synthesizes a `RuntimeDone` event from the platform.report metrics.
    #[allow(clippy::too_many_arguments)]
    async fn handle_managed_instance_report(
        &mut self,
        request_id: &String,
        metric: ManagedInstanceReportMetrics,
        timestamp: i64,
        status: Status,
        error_type: Option<String>,
        spans: Option<Vec<crate::extension::telemetry::events::TelemetrySpan>>,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
    ) {
        // Managed Instance mode doesn't have platform.runtimeDone event, so we synthesize it from platform.report
        // Try to get duration from responseLatency span, otherwise fall back to metric.duration_ms
        let duration_ms = spans
            .as_ref()
            .and_then(|span_vec| {
                span_vec
                    .iter()
                    .find(|span| span.name == "responseLatency")
                    .map(|span| span.duration_ms)
            })
            .unwrap_or(metric.duration_ms);

        let runtime_done_metrics = RuntimeDoneMetrics {
            duration_ms,
            produced_bytes: None,
        };

        // Track concurrent invocations - decrement after handling report
        if self.active_invocations > 0 {
            self.active_invocations -= 1;
        } else {
            debug!("Active invocations already at 0, not updating active invocations");
        }

        // Only pause monitoring when there are no active invocations
        if self.active_invocations == 0 {
            debug!("No active invocations, pausing usage metrics monitoring");
            self.enhanced_metrics.pause_usage_metrics_monitoring();
        }

        // Only process if we have context for this request
        if self.context_buffer.get(request_id).is_some() {
            self.on_platform_runtime_done(
                request_id,
                runtime_done_metrics,
                status,
                error_type,
                tags_provider,
                trace_sender,
                timestamp,
            )
            .await;
        } else {
            debug!(
                "Received platform report for unknown request_id: {}",
                request_id
            );
        }
    }

    /// Handles `OnDemand` mode platform report processing.
    ///
    /// Processes OnDemand-specific metrics including OOM detection for provided.al runtimes
    /// and post-runtime duration calculation.
    fn handle_ondemand_report(
        &mut self,
        request_id: &String,
        metrics: OnDemandReportMetrics,
        timestamp: i64,
    ) {
        // For provided.al runtimes, if the last invocation hit the memory limit, increment the OOM metric.
        // We do this for provided.al runtimes because we didn't find another way to detect this under provided.al.
        // We don't do this for other runtimes to avoid double counting.
        if let Some(runtime) = &self.runtime
            && runtime.starts_with("provided.al")
            && metrics.max_memory_used_mb == metrics.memory_size_mb
        {
            debug!(
                "Invocation Processor | PlatformReport | Last invocation hit memory limit. Incrementing OOM metric."
            );
            self.enhanced_metrics.increment_oom_metric(timestamp);
        }

        // Calculate and set post-runtime duration if context is available
        if let Some(context) = self.context_buffer.get(request_id)
            && context.runtime_duration_ms != 0.0
        {
            let post_runtime_duration_ms = metrics.duration_ms - context.runtime_duration_ms;
            self.enhanced_metrics
                .set_post_runtime_duration_metric(post_runtime_duration_ms, timestamp);
        }
    }

    pub fn on_shutdown_event(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs();
        self.enhanced_metrics
            .set_shutdown_metric(i64::try_from(now).expect("can't convert now to i64"));
        self.enhanced_metrics
            .set_unused_init_metric(i64::try_from(now).expect("can't convert now to i64"));

        // In managed instance mode, emit sandbox-level usage metrics at shutdown
        if self.aws_config.is_managed_instance_mode() {
            self.enhanced_metrics.set_max_enhanced_metrics(); // Emit system limits (fd_max, threads_max, tmp_max)
            self.enhanced_metrics.set_usage_enhanced_metrics(); // Emit sandbox-wide peak usage
        }
    }

    /// If this method is called, it means that we are operating in a Universally Instrumented
    /// runtime. Therefore, we need to set the `tracer_detected` flag to `true`.
    ///
    pub fn on_universal_instrumentation_start(
        &mut self,
        headers: HashMap<String, String>,
        payload_value: Value,
        request_id: Option<String>,
    ) {
        self.tracer_detected = true;

        self.inferrer.infer_span(&payload_value, &self.aws_config);

        // MANAGED INSTANCE MODE: Use request ID-based pairing for concurrent invocations
        if self.aws_config.is_managed_instance_mode() {
            if let Some(req_id) = request_id {
                debug!(
                    "Managed Instance: Processing UniversalInstrumentationStart for request_id: {}",
                    req_id
                );
                if self
                    .context_buffer
                    .pair_universal_instrumentation_start_with_request_id(
                        &req_id,
                        &headers,
                        &payload_value,
                    )
                {
                    // Invoke event already happened, process immediately
                    self.process_on_universal_instrumentation_start(req_id, headers, payload_value);
                } else {
                    // Buffered for later pairing when Invoke event arrives
                    debug!(
                        "Managed Instance: Buffered UniversalInstrumentationStart for request_id: {}",
                        req_id
                    );
                }
                return;
            }
            // Missing request_id in managed instance mode - log warning and fall back to FIFO
            warn!(
                "Managed Instance: UniversalInstrumentationStart missing request_id header. \
                Falling back to FIFO pairing (may cause incorrect pairing with concurrent invocations)"
            );
        }

        // ON-DEMAND MODE: Use existing FIFO pairing logic (unchanged)
        if let Some(request_id) = self
            .context_buffer
            .pair_universal_instrumentation_start_event(&headers, &payload_value)
        {
            self.process_on_universal_instrumentation_start(request_id, headers, payload_value);
        }
    }

    fn process_on_universal_instrumentation_start(
        &mut self,
        request_id: String,
        headers: HashMap<String, String>,
        payload_value: Value,
    ) {
        let Some(context) = self.context_buffer.get_mut(&request_id) else {
            debug!("Cannot process on invocation start, no context for request_id: {request_id}");
            return;
        };

        // Tag the invocation span with the request payload
        if self.config.capture_lambda_payload {
            let metadata = get_metadata_from_value(
                "function.request",
                &payload_value,
                0,
                self.config.capture_lambda_payload_max_depth,
            );
            context.invocation_span.meta.extend(metadata);
        }

        context.extracted_span_context =
            Self::extract_span_context(&headers, &payload_value, Arc::clone(&self.propagator));

        // Set the extracted trace context to the spans
        if let Some(sc) = &context.extracted_span_context {
            context.invocation_span.trace_id = sc.trace_id;
            context.invocation_span.parent_id = sc.span_id;

            // Set the right data to the correct root level span,
            // If there's an inferred span, then that should be the root.
            if self.inferrer.inferred_span.is_some() {
                self.inferrer.set_parent_id(sc.span_id);
                self.inferrer.extend_meta(sc.tags.clone());
            } else {
                context.invocation_span.meta.extend(sc.tags.clone());
            }
        }

        // If we have an inferred span, set the invocation span parent id
        // to be the inferred span id, even if we don't have an extracted trace context
        if let Some(inferred_span) = &self.inferrer.inferred_span {
            context.invocation_span.parent_id = inferred_span.span_id;
        }
    }

    pub fn add_reparenting(&mut self, request_id: String, span_id: u64, parent_id: u64) {
        for rep_info in &self.context_buffer.sorted_reparenting_info {
            if rep_info.request_id == request_id {
                warn!("Reparenting already exists for request_id: {request_id}, ignoring new one");
                return;
            }
        }
        if self.context_buffer.sorted_reparenting_info.len()
            == self.context_buffer.sorted_reparenting_info.capacity()
        {
            self.context_buffer.sorted_reparenting_info.pop_front();
        }

        self.context_buffer
            .sorted_reparenting_info
            .push_back(ReparentingInfo {
                request_id: request_id.clone(),
                invocation_span_id: span_id,
                parent_id_to_reparent: parent_id,
                guessed_trace_id: 0,
                needs_trace_id: true,
            });
    }

    #[must_use]
    pub fn get_reparenting_info(&self) -> VecDeque<ReparentingInfo> {
        self.context_buffer.sorted_reparenting_info.clone()
    }

    pub fn update_reparenting(
        &mut self,
        reparenting_info: VecDeque<ReparentingInfo>,
    ) -> Vec<Context> {
        let mut ctx_to_send = Vec::new();
        for rep_info in reparenting_info {
            if let Some(ctx) = self.context_buffer.get_mut(&rep_info.request_id) {
                let mut span_updated = false;
                if ctx.invocation_span.span_id == 0 {
                    ctx.invocation_span.span_id = rep_info.invocation_span_id;
                    debug!(
                        "Set invocation span id to {} for request_id: {}",
                        rep_info.guessed_trace_id, rep_info.request_id
                    );
                    span_updated = true;
                }
                if ctx.invocation_span.trace_id == 0 {
                    ctx.invocation_span.trace_id = rep_info.guessed_trace_id;
                    debug!(
                        "Set trace id to {} for request_id: {}",
                        rep_info.guessed_trace_id, rep_info.request_id
                    );
                    span_updated = true;
                }
                if span_updated
                    && ctx.invocation_span.span_id != 0
                    && ctx.invocation_span.trace_id != 0
                    && ctx.runtime_done_received
                {
                    ctx_to_send.push(ctx.clone());
                }
            } else {
                warn!(
                    "Mismatched request info. Context not found for request_id: {}",
                    rep_info.request_id
                );
            }

            if let Some(existing_info) = self
                .context_buffer
                .sorted_reparenting_info
                .iter_mut()
                .find(|info| info.request_id == rep_info.request_id)
            {
                existing_info.needs_trace_id = rep_info.needs_trace_id;
                existing_info.guessed_trace_id = rep_info.guessed_trace_id;
            }
        }
        ctx_to_send
    }

    pub fn extract_span_context(
        headers: &HashMap<String, String>,
        payload_value: &Value,
        propagator: Arc<impl Propagator>,
    ) -> Option<SpanContext> {
        if let Some(sc) =
            span_inferrer::extract_span_context(payload_value, Arc::clone(&propagator))
        {
            return Some(sc);
        }

        if let Some(sc) = payload_value
            .get("request")
            .and_then(|req| req.get("headers"))
            .and_then(|headers| propagator.extract(headers))
        {
            debug!("Extracted trace context from event.request.headers");
            return Some(sc);
        }

        if let Some(payload_headers) = payload_value.get("headers")
            && let Some(sc) = propagator.extract(payload_headers)
        {
            debug!("Extracted trace context from event headers");
            return Some(sc);
        }

        if let Some(sc) = propagator.extract(headers) {
            debug!("Extracted trace context from headers");
            return Some(sc);
        }

        None
    }

    /// Given trace context information, set it to the current span.
    ///
    pub fn on_universal_instrumentation_end(
        &mut self,
        headers: HashMap<String, String>,
        payload_value: Value,
        request_id: Option<String>,
    ) {
        // MANAGED INSTANCE MODE: Use request ID-based pairing for concurrent invocations
        if self.aws_config.is_managed_instance_mode() {
            if let Some(req_id) = request_id {
                debug!(
                    "Managed Instance: Processing UniversalInstrumentationEnd for request_id: {}",
                    req_id
                );
                if self
                    .context_buffer
                    .pair_universal_instrumentation_end_with_request_id(
                        &req_id,
                        &headers,
                        &payload_value,
                    )
                {
                    // PlatformRuntimeDone already happened, process immediately
                    self.process_on_universal_instrumentation_end(req_id, headers, payload_value);
                } else {
                    // Buffered for later pairing when PlatformRuntimeDone arrives
                    debug!(
                        "Managed Instance: Buffered UniversalInstrumentationEnd for request_id: {}",
                        req_id
                    );
                }
                return;
            }
            // Missing request_id in managed instance mode - log warning and fall back to FIFO
            warn!(
                "Managed Instance: UniversalInstrumentationEnd missing request_id header. \
                Falling back to FIFO pairing (may cause incorrect pairing with concurrent invocations)"
            );
        }

        // ON-DEMAND MODE: Use existing FIFO pairing logic (unchanged)
        // If `PlatformRuntimeDone` event happened first, process
        if let Some(request_id) = self
            .context_buffer
            .pair_universal_instrumentation_end_event(&headers, &payload_value)
        {
            self.process_on_universal_instrumentation_end(request_id, headers, payload_value);
        }
    }

    fn process_on_universal_instrumentation_end(
        &mut self,
        request_id: String,
        headers: HashMap<String, String>,
        payload_value: Value,
    ) {
        let Some(context) = self.context_buffer.get_mut(&request_id) else {
            debug!("Cannot process on invocation end, no context for request_id: {request_id}");
            return;
        };

        // Tag the invocation span with the request payload
        if self.config.capture_lambda_payload {
            let metadata = get_metadata_from_value(
                "function.response",
                &payload_value,
                0,
                self.config.capture_lambda_payload_max_depth,
            );
            context.invocation_span.meta.extend(metadata);
        }

        if let Some(status_code) = payload_value.get("statusCode").and_then(Value::as_i64) {
            let status_code_as_string = status_code.to_string();
            context.invocation_span.meta.insert(
                "http.status_code".to_string(),
                status_code_as_string.clone(),
            );

            if status_code_as_string.len() == 3 && status_code_as_string.starts_with('5') {
                context.invocation_span.error = 1;
            }

            // If we have an inferred span, set the status code to it
            self.inferrer.set_status_code(status_code_as_string);
        }

        let mut trace_id = 0;
        let mut parent_id = 0;
        let mut tags: HashMap<String, String> = HashMap::new();

        // If we have a trace context, this means we got it from
        // distributed tracing
        if let Some(sc) = &context.extracted_span_context {
            trace_id = sc.trace_id;
            parent_id = sc.span_id;
            tags.extend(sc.tags.clone());
        }

        // We are the root span, so we should extract the trace context
        // from the tracer, which has sent it through end invocation headers
        if trace_id == 0 {
            // Extract trace context from headers manually
            if let Some(header) = headers.get(DATADOG_TRACE_ID_KEY) {
                trace_id = header.parse::<u64>().unwrap_or(0);
            }

            if let Some(header) = headers.get(DATADOG_PARENT_ID_KEY) {
                parent_id = header.parse::<u64>().unwrap_or(0);
            }

            if let Some(priority_str) = headers.get(DATADOG_SAMPLING_PRIORITY_KEY)
                && let Ok(priority) = priority_str.parse::<f64>()
            {
                context
                    .invocation_span
                    .metrics
                    .insert(TAG_SAMPLING_PRIORITY.to_string(), priority);
            }

            // Extract tags from headers
            // Used for 128 bit trace ids
            tags = DatadogHeaderPropagator::extract_tags(&headers);
        }

        // We should always use the generated span id from the tracer
        if let Some(header) = headers.get(DATADOG_SPAN_ID_KEY) {
            context.invocation_span.span_id = header.parse::<u64>().unwrap_or(0);
        }

        if trace_id == 0 {
            trace_id = context.invocation_span.trace_id;
        }
        context.invocation_span.trace_id = trace_id;

        if self.inferrer.inferred_span.is_some() {
            self.inferrer.extend_meta(tags);
        } else {
            context.invocation_span.parent_id = parent_id;
            context.invocation_span.meta.extend(tags);
        }

        if let Some(error_tags) =
            Self::get_error_tags_from_headers(headers, context.invocation_span.error == 1)
        {
            context.invocation_span.meta.extend(error_tags);
            context.invocation_span.error = 1;
        }

        if context.invocation_span.error == 1 {
            let now = std::time::UNIX_EPOCH
                .elapsed()
                .expect("can't poll clock")
                .as_secs()
                .try_into()
                .unwrap_or_default();
            self.enhanced_metrics.increment_errors_metric(now);
        }
    }

    /// Given a set of end invocation headers, get error metadata from them.
    ///
    #[must_use]
    pub fn get_error_tags_from_headers(
        headers: HashMap<String, String>,
        has_error: bool,
    ) -> Option<HashMap<String, String>> {
        let message = headers.get(DATADOG_INVOCATION_ERROR_MESSAGE_KEY);
        let r#type = headers.get(DATADOG_INVOCATION_ERROR_TYPE_KEY);
        let stack = headers.get(DATADOG_INVOCATION_ERROR_STACK_KEY);

        let is_error = headers
            .get(DATADOG_INVOCATION_ERROR_KEY)
            .is_some_and(|v| v.to_lowercase() == "true")
            || message.is_some()
            || stack.is_some()
            || r#type.is_some()
            || has_error;

        if !is_error {
            return None;
        }

        let mut error_tags = HashMap::<String, String>::new();
        if let Some(m) = message {
            let decoded_message = base64_to_string(m).unwrap_or_else(|_| {
                debug!("Error message header may not be encoded, setting as is");
                m.clone()
            });

            error_tags.insert(String::from("error.msg"), decoded_message);
        }

        if let Some(t) = r#type {
            let decoded_type = base64_to_string(t).unwrap_or_else(|_| {
                debug!("Error type header may not be encoded, setting as is");
                t.clone()
            });

            error_tags.insert(String::from("error.type"), decoded_type);
        }

        if let Some(s) = stack {
            let decoded_stack = base64_to_string(s).unwrap_or_else(|e| {
                debug!("Failed to decode error stack: {e}");
                s.clone()
            });

            error_tags.insert(String::from("error.stack"), decoded_stack);
        }

        Some(error_tags)
    }

    pub fn on_out_of_memory_error(&mut self, timestamp: i64) {
        self.enhanced_metrics.increment_oom_metric(timestamp);
    }

    /// Add a tracer span to the context buffer for the given `request_id`, if present.
    ///
    /// This is used to enrich the invocation span with additional metadata from the tracers
    /// top level span, since we discard the tracer span when we create the invocation span.
    pub fn add_tracer_span(&mut self, span: &Span) {
        if let Some(request_id) = span.meta.get("request_id") {
            self.context_buffer.add_tracer_span(request_id, span);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::LAMBDA_RUNTIME_SLUG;
    use crate::extension::telemetry::events::ManagedInstanceReportMetrics;
    use crate::traces::stats_concentrator_service::StatsConcentratorService;
    use crate::traces::stats_generator::StatsGenerator;
    use crate::traces::trace_processor;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use dogstatsd::aggregator_service::AggregatorService;
    use dogstatsd::metric::EMPTY_TAGS;
    use serde_json::json;

    fn setup() -> Processor {
        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".into(),
            aws_lwa_proxy_lambda_runtime_api: Some("***".into()),
            function_name: "test-function".into(),
            sandbox_init_time: Instant::now(),
            runtime_api: "***".into(),
            exec_wrapper: None,
            initialization_type: "on-demand".into(),
        });

        let config = Arc::new(config::Config {
            service: Some("test-service".to_string()),
            tags: HashMap::from([("test".to_string(), "tags".to_string())]),
            ..config::Config::default()
        });

        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
        ));

        let (service, handle) =
            AggregatorService::new(EMPTY_TAGS, 1024).expect("failed to create aggregator service");

        tokio::spawn(service.run());

        let propagator = Arc::new(DatadogCompositePropagator::new(Arc::clone(&config)));
        Processor::new(tags_provider, config, aws_config, handle, propagator)
    }

    #[test]
    fn test_get_error_tags_from_headers_base64_encoded_headers() {
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

        let error_tags =
            Processor::get_error_tags_from_headers(headers, false).expect("error tags not found");

        assert_eq!(error_tags["error.msg"], error_message);
        assert_eq!(error_tags["error.type"], error_type);
        assert_eq!(error_tags["error.stack"], error_stack);
    }

    #[test]
    fn test_get_error_tags_from_headers_non_encoded_headers() {
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

        let error_tags =
            Processor::get_error_tags_from_headers(headers, false).expect("error tags not found");

        assert_eq!(error_tags["error.msg"], error_message);
        assert_eq!(error_tags["error.type"], error_type);
        assert_eq!(error_tags["error.stack"], error_stack);
    }

    #[tokio::test]
    async fn test_process_on_universal_instrumentation_end_headers_with_sampling_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "999".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "1000".to_string());
        headers.insert(DATADOG_SAMPLING_PRIORITY_KEY.to_string(), "-1".to_string());

        let request_id = String::from("request_id");
        p.context_buffer.start_context(&request_id, Span::default());

        p.process_on_universal_instrumentation_end(request_id.clone(), headers, json!({}));

        let context = p
            .context_buffer
            .get(&request_id)
            .expect("context not found");

        assert_eq!(context.invocation_span.trace_id, 999);
        assert_eq!(context.invocation_span.parent_id, 1000);
        let priority = context
            .invocation_span
            .metrics
            .get(TAG_SAMPLING_PRIORITY)
            .copied();
        assert_eq!(priority, Some(-1.0));
    }

    #[tokio::test]
    async fn test_process_on_universal_instrumentation_end_headers_with_invalid_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "888".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "999".to_string());
        headers.insert(
            DATADOG_SAMPLING_PRIORITY_KEY.to_string(),
            "not-a-number".to_string(),
        );

        let request_id = String::from("request_id");
        p.context_buffer.start_context(&request_id, Span::default());

        p.process_on_universal_instrumentation_end(request_id.clone(), headers, json!({}));

        let context = p.context_buffer.get(&request_id).unwrap();

        assert!(
            !context
                .invocation_span
                .metrics
                .contains_key(TAG_SAMPLING_PRIORITY)
        );
        assert_eq!(context.invocation_span.trace_id, 888);
        assert_eq!(context.invocation_span.parent_id, 999);
    }

    #[tokio::test]
    async fn test_process_on_universal_instrumentation_end_headers_no_sampling_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "111".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "222".to_string());

        let request_id = String::from("request_id");
        p.context_buffer.start_context(&request_id, Span::default());

        p.process_on_universal_instrumentation_end(request_id.clone(), headers, json!({}));

        let context = p.context_buffer.get(&request_id).unwrap();

        assert!(
            !context
                .invocation_span
                .metrics
                .contains_key(TAG_SAMPLING_PRIORITY)
        );
        assert_eq!(context.invocation_span.trace_id, 111);
        assert_eq!(context.invocation_span.parent_id, 222);
    }

    #[tokio::test]
    async fn test_process_on_invocation_end_tags_response_with_status_code() {
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

        let request_id = String::from("request_id");
        p.context_buffer.start_context(&request_id, Span::default());
        p.context_buffer.add_start_time(&request_id, 1);
        let parsed_response = serde_json::from_str(response).unwrap();
        p.process_on_universal_instrumentation_end(
            request_id.clone(),
            HashMap::new(),
            parsed_response,
        );

        let context = p.context_buffer.get(&request_id).unwrap();

        assert_eq!(
            context
                .invocation_span
                .meta
                .get("http.status_code")
                .expect("Status code not parsed!"),
            "200"
        );
    }

    #[tokio::test]
    async fn test_on_shutdown_event_creates_unused_init_metrics() {
        let mut processor = setup();

        let now1 = i64::try_from(std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()).unwrap();
        let ts1 = (now1 / 10) * 10;
        processor.on_shutdown_event();
        let now2 = i64::try_from(std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()).unwrap();
        let ts2 = (now2 / 10) * 10;

        let handle = &processor.enhanced_metrics.aggr_handle;

        assert!(
            handle
                .get_entry_by_id(
                    crate::metrics::enhanced::constants::UNUSED_INIT.into(),
                    None,
                    ts1
                )
                .await
                .unwrap()
                .is_some()
                || handle
                    .get_entry_by_id(
                        crate::metrics::enhanced::constants::UNUSED_INIT.into(),
                        None,
                        ts2
                    )
                    .await
                    .unwrap()
                    .is_some(),
            "UNUSED_INIT metric should be created when invoked_received=false"
        );
    }

    macro_rules! platform_report_managed_instance_tests {
        ($($name:ident: $value:expr,)*) => {
            $(
                #[tokio::test]
                async fn $name() {
                    use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;

                    let (
                        request_id,
                        setup_context,
                        duration_ms,
                        status,
                        error_type,
                        should_have_context_after,
                    ): (&str, bool, f64, Status, Option<String>, bool) = $value;

                    let mut processor = setup();

                    // Setup context if needed
                    if setup_context {
                        processor.on_invoke_event(request_id.to_string());
                        let start_time = chrono::Utc::now();
                        processor.on_platform_start(request_id.to_string(), start_time);
                    }

                    let metrics = ReportMetrics::ManagedInstance(ManagedInstanceReportMetrics {
                        duration_ms,
                    });

                    // Create tags_provider
                    let config = Arc::new(config::Config {
                        service: Some("test-service".to_string()),
                        tags: HashMap::from([("test".to_string(), "tags".to_string())]),
                        ..config::Config::default()
                    });
                    let tags_provider = Arc::new(provider::Provider::new(
                        Arc::clone(&config),
                        LAMBDA_RUNTIME_SLUG.to_string(),
                        &HashMap::from([("function_arn".to_string(), "test-arn".to_string())]),
                    ));

                    // Create stats concentrator for test
                    let (stats_concentrator_service, stats_concentrator_handle) =
                        StatsConcentratorService::new(Arc::clone(&config));
                    tokio::spawn(stats_concentrator_service.run());

                    // Create trace sender
                    let trace_sender = Arc::new(SendingTraceProcessor {
                        appsec: None,
                        processor: Arc::new(trace_processor::ServerlessTraceProcessor {
                            obfuscation_config: Arc::new(ObfuscationConfig::new().expect("Failed to create ObfuscationConfig")),
                        }),
                        trace_tx: tokio::sync::mpsc::channel(1).0,
                        stats_generator: Arc::new(StatsGenerator::new(stats_concentrator_handle)),
                    });

                    // Call on_platform_report
                    let request_id_string = request_id.to_string();
                    processor.on_platform_report(
                        &request_id_string,
                        metrics,
                        chrono::Utc::now().timestamp(),
                        status,
                        error_type,
                        None, // spans
                        tags_provider,
                        trace_sender,
                    ).await;

                    // Verify context state
                    let request_id_string_for_get = request_id.to_string();
                    assert_eq!(
                        processor.context_buffer.get(&request_id_string_for_get).is_some(),
                        should_have_context_after,
                        "Context existence mismatch for request_id: {}",
                        request_id
                    );
                }
            )*
        }
    }

    platform_report_managed_instance_tests! {
        // (request_id, setup_context, duration_ms, status, error_type, should_have_context_after)
        test_on_platform_report_managed_instance_mode_with_valid_context: (
            "test-request-id",
            true,  // setup context
            123.45,
            Status::Success,
            None,
            true,  // context should still exist
        ),

        test_on_platform_report_managed_instance_mode_without_context: (
            "unknown-request-id",
            false, // no context setup
            123.45,
            Status::Success,
            None,
            false, // context should not exist
        ),

        test_on_platform_report_managed_instance_mode_with_error_status: (
            "test-request-id-error",
            true,  // setup context
            200.0,
            Status::Error,
            Some("RuntimeError".to_string()),
            true,  // context should still exist
        ),

        test_on_platform_report_managed_instance_mode_with_timeout: (
            "test-request-id-timeout",
            true,  // setup context
            30000.0,
            Status::Timeout,
            None,
            true,  // context should still exist
        ),
    }

    #[tokio::test]
    async fn test_on_platform_init_start_sets_durable_function_tag() {
        let mut processor = setup();
        let time = Utc::now();

        processor.on_platform_init_start(
            time,
            Some("python:3.14.DurableFunction.v6".to_string()),
        );

        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        processor.enhanced_metrics.increment_invocation_metric(now);

        let ts = (now / 10) * 10;
        let durable_tags = dogstatsd::metric::SortedTags::parse("durable_function:true").ok();
        let entry = processor
            .enhanced_metrics
            .aggr_handle
            .get_entry_by_id(
                crate::metrics::enhanced::constants::INVOCATIONS_METRIC.into(),
                durable_tags,
                ts,
            )
            .await
            .unwrap();
        assert!(
            entry.is_some(),
            "Expected durable_function:true tag on enhanced metric"
        );
    }

    #[tokio::test]
    async fn test_on_platform_init_start_no_durable_function_tag_for_regular_runtime() {
        let mut processor = setup();
        let time = Utc::now();

        processor.on_platform_init_start(time, Some("python:3.12.v10".to_string()));

        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        processor.enhanced_metrics.increment_invocation_metric(now);

        let ts = (now / 10) * 10;
        let durable_tags = dogstatsd::metric::SortedTags::parse("durable_function:true").ok();
        let entry = processor
            .enhanced_metrics
            .aggr_handle
            .get_entry_by_id(
                crate::metrics::enhanced::constants::INVOCATIONS_METRIC.into(),
                durable_tags,
                ts,
            )
            .await
            .unwrap();
        assert!(
            entry.is_none(),
            "Expected no durable_function:true tag for regular runtime"
        );
    }

    #[tokio::test]
    async fn test_on_platform_init_start_no_durable_function_tag_when_runtime_version_is_none() {
        let mut processor = setup();
        let time = Utc::now();

        processor.on_platform_init_start(time, None);

        let now: i64 = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        processor.enhanced_metrics.increment_invocation_metric(now);

        let ts = (now / 10) * 10;
        let durable_tags = dogstatsd::metric::SortedTags::parse("durable_function:true").ok();
        let entry = processor
            .enhanced_metrics
            .aggr_handle
            .get_entry_by_id(
                crate::metrics::enhanced::constants::INVOCATIONS_METRIC.into(),
                durable_tags,
                ts,
            )
            .await
            .unwrap();
        assert!(
            entry.is_none(),
            "Expected no durable_function:true tag when runtime_version is None"
        );
    }

    #[tokio::test]
    async fn test_is_managed_instance_mode_returns_true() {
        use crate::config::aws::LAMBDA_MANAGED_INSTANCES_INIT_TYPE;

        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".into(),
            aws_lwa_proxy_lambda_runtime_api: Some("***".into()),
            function_name: "test-function".into(),
            sandbox_init_time: Instant::now(),
            runtime_api: "***".into(),
            exec_wrapper: None,
            initialization_type: LAMBDA_MANAGED_INSTANCES_INIT_TYPE.into(), // Managed Instance mode
        });

        let config = Arc::new(config::Config::default());
        let tags_provider = Arc::new(provider::Provider::new(
            Arc::clone(&config),
            LAMBDA_RUNTIME_SLUG.to_string(),
            &HashMap::new(),
        ));

        let (service, handle) =
            AggregatorService::new(EMPTY_TAGS, 1024).expect("failed to create aggregator service");
        tokio::spawn(service.run());

        let propagator = Arc::new(DatadogCompositePropagator::new(Arc::clone(&config)));

        let processor = Processor::new(tags_provider, config, aws_config, handle, propagator);

        assert!(
            processor.is_managed_instance_mode(),
            "Should be in Managed Instance mode"
        );
    }

    #[tokio::test]
    async fn test_is_managed_instance_mode_returns_false() {
        let processor = setup(); // Uses "on-demand" initialization_type
        assert!(
            !processor.is_managed_instance_mode(),
            "Should not be in managed instance mode"
        );
    }

    #[tokio::test]
    async fn test_on_platform_restore_start_creates_snapstart_span() {
        let mut processor = setup();
        let request_id = String::from("test-request-id");

        // Create a context first
        processor
            .context_buffer
            .start_context(&request_id, Span::default());

        // Simulate platform restore start
        let time = Utc::now();
        processor.on_platform_restore_start(time);

        // Get the closest context (should be our test context)
        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();

        let context = processor
            .context_buffer
            .get_closest_mut(start_time)
            .unwrap();

        // Assert that snapstart_restore_span was created
        assert!(context.snapstart_restore_span.is_some());

        let snapstart_span = context.snapstart_restore_span.as_ref().unwrap();
        assert_eq!(snapstart_span.name, "aws.lambda.snapstart_restore");
        assert_eq!(snapstart_span.start, start_time);
        assert_ne!(snapstart_span.span_id, 0);
    }

    #[tokio::test]
    async fn test_on_platform_restore_start_no_context() {
        let mut processor = setup();

        // Call on_platform_restore_start without creating a context first
        let time = Utc::now();
        processor.on_platform_restore_start(time);

        // Should not panic, just log a debug message
        // Test passes if no panic occurs
    }

    #[tokio::test]
    async fn test_get_ctx_spans_prioritizes_snapstart_over_cold_start() {
        let mut processor = setup();
        let request_id = String::from("test-request-id");

        // Create invocation span
        let invocation_span = Span {
            name: "aws.lambda".to_string(),
            span_id: 1,
            trace_id: 100,
            ..Default::default()
        };

        // Create cold start span
        let cold_start_span = Span {
            name: "aws.lambda.cold_start".to_string(),
            span_id: 2,
            trace_id: 100,
            ..Default::default()
        };

        // Create snapstart restore span
        let snapstart_span = Span {
            name: "aws.lambda.snapstart_restore".to_string(),
            span_id: 3,
            trace_id: 100,
            ..Default::default()
        };

        // Build context with both cold start and snapstart spans
        let mut context = Context::from_request_id(&request_id);
        context.invocation_span = invocation_span.clone();
        context.cold_start_span = Some(cold_start_span.clone());
        context.snapstart_restore_span = Some(snapstart_span.clone());

        // Call get_ctx_spans to get the spans that would be sent
        let (spans, _body_size) = processor.get_ctx_spans(context);

        // Verify that exactly 2 spans are returned:
        // 1. invocation_span
        // 2. snapstart_restore_span (NOT cold_start_span)
        assert_eq!(spans.len(), 2, "Expected 2 spans (invocation + snapstart)");

        // Verify the first span is the invocation span
        assert_eq!(spans[0].name, "aws.lambda");
        assert_eq!(spans[0].span_id, 1);

        // Verify the second span is the snapstart span, NOT the cold start span
        assert_eq!(spans[1].name, "aws.lambda.snapstart_restore");
        assert_eq!(
            spans[1].span_id, 3,
            "Should be snapstart span (id=3), not cold start span (id=2)"
        );
    }

    #[test]
    fn test_extract_span_context_priority_order() {
        let config = Arc::new(config::Config {
            trace_propagation_style_extract: vec![
                config::trace_propagation_style::TracePropagationStyle::Datadog,
                config::trace_propagation_style::TracePropagationStyle::TraceContext,
            ],
            ..config::Config::default()
        });
        let propagator = Arc::new(DatadogCompositePropagator::new(Arc::clone(&config)));

        let mut headers = HashMap::new();
        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "111".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "222".to_string());

        let payload = json!({
            "headers": {
                "x-datadog-trace-id": "333",
                "x-datadog-parent-id": "444"
            },
            "request": {
                "headers": {
                    "x-datadog-trace-id": "555",
                    "x-datadog-parent-id": "666"
                }
            }
        });

        let result = Processor::extract_span_context(&headers, &payload, propagator);

        assert!(result.is_some());
        let context = result.unwrap();
        assert_eq!(
            context.trace_id, 555,
            "Should prioritize event.request.headers as service-specific extraction"
        );
    }

    #[test]
    fn test_extract_span_context_no_request_headers() {
        let config = Arc::new(config::Config {
            trace_propagation_style_extract: vec![
                config::trace_propagation_style::TracePropagationStyle::Datadog,
                config::trace_propagation_style::TracePropagationStyle::TraceContext,
            ],
            ..config::Config::default()
        });
        let propagator = Arc::new(DatadogCompositePropagator::new(Arc::clone(&config)));
        let headers = HashMap::new();

        let payload = json!({
            "argumentsMap": {
                "id": "123"
            },
            "request": {
                "body": "some body"
            }
        });

        let result = Processor::extract_span_context(&headers, &payload, propagator);

        assert!(
            result.is_none(),
            "Should return None when no trace context found"
        );
    }
}
