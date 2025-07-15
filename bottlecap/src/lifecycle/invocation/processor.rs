use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use datadog_trace_protobuf::pb::Span;
use datadog_trace_utils::{send_data::SendData, tracer_header_tags};
use dogstatsd::aggregator::Aggregator as MetricsAggregator;
use serde_json::{json, Value};
use tokio::sync::{mpsc::Sender, watch};
use tracing::{debug, warn};

use crate::appsec::processor::AppSecContext;
use crate::{
    config::{self, aws::AwsConfig},
    lifecycle::invocation::{
        base64_to_string, context::Context, context::ContextBuffer, context::ReparentingInfo,
        create_empty_span, generate_span_id, get_metadata_from_value, span_inferrer::SpanInferrer,
    },
    metrics::enhanced::lambda::{EnhancedMetricData, Lambda as EnhancedMetrics},
    proc::{
        self,
        constants::{ETC_PATH, PROC_PATH},
        CPUData, NetworkData,
    },
    tags::{lambda::tags::resolve_runtime_from_proc, provider},
    telemetry::events::{InitType, ReportMetrics, RuntimeDoneMetrics, Status},
    traces::{
        context::SpanContext,
        propagation::{
            text_map_propagator::{
                DatadogHeaderPropagator, DATADOG_PARENT_ID_KEY, DATADOG_SAMPLING_PRIORITY_KEY,
                DATADOG_SPAN_ID_KEY, DATADOG_TRACE_ID_KEY,
            },
            DatadogCompositePropagator, Propagator,
        },
        trace_processor::{self, TraceProcessor},
    },
};

pub const MS_TO_NS: f64 = 1_000_000.0;
pub const S_TO_NS: f64 = 1_000_000_000.0;
pub const PROACTIVE_INITIALIZATION_THRESHOLD_MS: u64 = 10_000;

pub const DATADOG_INVOCATION_ERROR_MESSAGE_KEY: &str = "x-datadog-invocation-error-msg";
pub const DATADOG_INVOCATION_ERROR_TYPE_KEY: &str = "x-datadog-invocation-error-type";
pub const DATADOG_INVOCATION_ERROR_STACK_KEY: &str = "x-datadog-invocation-error-stack";
pub const DATADOG_INVOCATION_ERROR_KEY: &str = "x-datadog-invocation-error";
pub const TAG_SAMPLING_PRIORITY: &str = "_sampling_priority_v1";

pub struct Processor {
    /// Buffer containing context of the previous 5 invocations
    context_buffer: ContextBuffer,
    /// Helper class to infer upstream span information and
    /// extract trace context if available.
    inferrer: SpanInferrer,
    /// Propagator to extract span context from carriers.
    propagator: DatadogCompositePropagator,
    /// Helper class to send enhanced metrics.
    enhanced_metrics: EnhancedMetrics,
    /// AWS configuration from the Lambda environment.
    aws_config: AwsConfig,
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
}

impl Processor {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
        aws_config: &AwsConfig,
        metrics_aggregator: Arc<Mutex<MetricsAggregator>>,
    ) -> Self {
        let service = config.service.clone().unwrap_or(String::from("aws.lambda"));
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or(String::from("aws.lambda"));

        let propagator = DatadogCompositePropagator::new(Arc::clone(&config));

        Processor {
            context_buffer: ContextBuffer::default(),
            inferrer: SpanInferrer::new(config.service_mapping.clone()),
            propagator,
            enhanced_metrics: EnhancedMetrics::new(metrics_aggregator, Arc::clone(&config)),
            aws_config: aws_config.clone(),
            tracer_detected: false,
            runtime: None,
            config: Arc::clone(&config),
            service,
            resource,
            dynamic_tags: HashMap::new(),
        }
    }

    /// Binds the provided security context to the invocation context for the given `request_id`.
    ///
    /// This may happen before [`on_invoke_event`][Self::on_invoke_event] is called and hence cannot assume any
    /// request-specific state has been initialized at this time.
    pub(crate) fn bind_security_context(&mut self, request_id: &str, context: AppSecContext) {
        self.context_buffer
            .bind_security_context(request_id, context);
    }

    /// Retrieves the security context bound to the given `request_id`, if one exists.
    pub(crate) fn get_security_context_mut(
        &mut self,
        request_id: &str,
    ) -> Option<&mut AppSecContext> {
        self.context_buffer.get_security_context_mut(request_id)
    }

    /// Given a `request_id`, creates the context and adds the enhanced metric offsets to the context buffer.
    ///
    pub fn on_invoke_event(&mut self, request_id: String) {
        let invocation_span =
            create_empty_span(String::from("aws.lambda"), &self.resource, &self.service);
        // Important! Call set_init_tags() before adding the invocation to the context buffer
        self.set_init_tags();
        self.context_buffer
            .start_context(&request_id, invocation_span);

        let timestamp = std::time::UNIX_EPOCH
            .elapsed()
            .expect("can't poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();

        if self.config.lambda_proc_enhanced_metrics {
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
            self.context_buffer
                .add_enhanced_metric_data(&request_id, enhanced_metric_offsets);
        }

        // Increment the invocation metric
        self.enhanced_metrics.increment_invocation_metric(timestamp);

        // If `UniversalInstrumentationStart` event happened first, process it
        if let Some((headers, payload)) = self.context_buffer.pair_invoke_event(&request_id) {
            let payload_value =
                serde_json::from_slice::<Value>(&payload).unwrap_or_else(|_| json!({}));
            // Infer span
            self.inferrer.infer_span(&payload_value, &self.aws_config);
            let span_context = self.extract_span_context(&headers, &payload_value);

            self.process_on_universal_instrumentation_start(request_id, payload, span_context);
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

        self.dynamic_tags
            .insert(String::from("cold_start"), cold_start.to_string());
        if proactive_initialization {
            self.dynamic_tags.insert(
                String::from("proactive_initialization"),
                proactive_initialization.to_string(),
            );
        }

        if let Some(runtime) = &self.runtime {
            self.dynamic_tags
                .insert(String::from("runtime"), runtime.to_string());
            self.enhanced_metrics.set_runtime_tag(runtime);
        }

        self.enhanced_metrics
            .set_init_tags(proactive_initialization, cold_start);
    }

    /// Called when the platform init starts.
    ///
    /// This is used to create a cold start span, since this telemetry event does not
    /// provide a `request_id`, we try to guess which invocation is the cold start.
    pub fn on_platform_init_start(&mut self, time: DateTime<Utc>) {
        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();

        // Get the closest context
        let Some(context) = self.context_buffer.get_closest_mut(start_time) else {
            debug!("Cannot process on platform init start, no invocation context found");
            return;
        };

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

        let Some(context) = self.context_buffer.get_closest_mut(timestamp) else {
            debug!("Cannot process on platform init report, no invocation context found");
            return;
        };

        if let Some(cold_start_span) = &mut context.cold_start_span {
            // `round` is intentionally meant to be a whole integer
            cold_start_span.duration = (duration_ms * MS_TO_NS) as i64;
        }
    }

    /// Given a `request_id` and the time of the platform start, add the start time to the context buffer.
    ///
    pub fn on_platform_start(&mut self, request_id: String, time: DateTime<Utc>) {
        let start_time: i64 = SystemTime::from(time)
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();
        self.context_buffer.add_start_time(&request_id, start_time);
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::cast_possible_truncation)]
    pub async fn on_platform_runtime_done(
        &mut self,
        request_id: &String,
        metrics: RuntimeDoneMetrics,
        status: Status,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_agent_tx: Sender<SendData>,
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
        }

        self.context_buffer
            .add_runtime_duration(request_id, metrics.duration_ms);

        // If `UniversalInstrumentationEnd` event happened first, process it first
        if let Some((headers, payload)) = self
            .context_buffer
            .pair_platform_runtime_done_event(request_id)
        {
            self.process_on_universal_instrumentation_end(request_id.clone(), headers, payload);
        }

        self.process_on_platform_runtime_done(
            request_id,
            status,
            tags_provider,
            trace_processor,
            trace_agent_tx,
        )
        .await;
    }

    async fn process_on_platform_runtime_done(
        &mut self,
        request_id: &String,
        status: Status,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_agent_tx: Sender<SendData>,
    ) {
        let context = self.enrich_ctx_at_platform_done(request_id, status);

        if self.tracer_detected {
            if let Some(ctx) = context {
                if ctx.invocation_span.trace_id != 0 && ctx.invocation_span.span_id != 0 {
                    self.send_ctx_spans(&tags_provider, &trace_processor, &trace_agent_tx, ctx)
                        .await;
                }
            }
        } else {
            self.send_cold_start_span(&tags_provider, &trace_processor, &trace_agent_tx)
                .await;
        }
    }

    fn enrich_ctx_at_platform_done(
        &mut self,
        request_id: &String,
        status: Status,
    ) -> Option<Context> {
        let Some(context) = self.context_buffer.get_mut(request_id) else {
            debug!("Cannot process on platform runtime done, no invocation context found for request_id: {request_id}");
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
            // Send the signal to stop monitoring tmp
            _ = offsets.tmp_chan_tx.send(());
            // Send the signal to stop monitoring file descriptors and threads
            _ = offsets.process_chan_tx.send(());
        }

        context.absorb_appsec_tags();

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
        if let Some(cold_start_span) = &mut context.cold_start_span {
            if context.invocation_span.trace_id != 0 {
                cold_start_span.trace_id = context.invocation_span.trace_id;
                cold_start_span.parent_id = context.invocation_span.parent_id;
            }
        }
        Some(context.clone())
    }

    pub async fn send_ctx_spans(
        &mut self,
        tags_provider: &Arc<provider::Provider>,
        trace_processor: &Arc<dyn TraceProcessor + Send + Sync>,
        trace_agent_tx: &Sender<SendData>,
        context: Context,
    ) {
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

        if let Some(cold_start_span) = &context.cold_start_span {
            body_size += std::mem::size_of_val(cold_start_span);
            traces.push(cold_start_span.clone());
        }

        self.send_spans(
            traces,
            body_size,
            tags_provider,
            trace_processor,
            trace_agent_tx,
        )
        .await;
    }

    /// For Node/Python: Updates the cold start span with the given trace ID.
    /// Returns the Span ID of the cold start span so we can reparent the `aws.lambda.load` span.
    pub fn set_cold_start_span_trace_id(&mut self, trace_id: u64) -> Option<u64> {
        if let Some(cold_start_context) = self.context_buffer.get_context_with_cold_start() {
            if let Some(cold_start_span) = &mut cold_start_context.cold_start_span {
                if cold_start_span.trace_id == 0 {
                    cold_start_span.trace_id = trace_id;
                }

                return Some(cold_start_span.span_id);
            }
        }

        None
    }

    /// For Node/Python: Sends the cold start span to the trace agent.
    async fn send_cold_start_span(
        &mut self,
        tags_provider: &Arc<provider::Provider>,
        trace_processor: &Arc<dyn TraceProcessor + Send + Sync>,
        trace_agent_tx: &Sender<SendData>,
    ) {
        if let Some(cold_start_context) = self.context_buffer.get_context_with_cold_start() {
            if let Some(cold_start_span) = &mut cold_start_context.cold_start_span {
                if cold_start_span.trace_id == 0 {
                    debug!("Not sending cold start span because trace ID is unset.");
                    return;
                }

                let traces = vec![cold_start_span.clone()];
                let body_size = size_of_val(cold_start_span);

                self.send_spans(
                    traces,
                    body_size,
                    tags_provider,
                    trace_processor,
                    trace_agent_tx,
                )
                .await;
            }
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
        trace_processor: &Arc<dyn TraceProcessor + Send + Sync>,
        trace_agent_tx: &Sender<SendData>,
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

        let send_data: SendData = trace_processor.process_traces(
            self.config.clone(),
            tags_provider.clone(),
            header_tags,
            vec![traces],
            body_size,
            self.inferrer.span_pointers.clone(),
        );

        if let Err(e) = trace_agent_tx.send(send_data).await {
            debug!("Failed to send context spans to agent: {e}");
        }
    }

    /// Given a `request_id` and the duration in milliseconds of the platform report,
    /// calculate the duration of the runtime if the `request_id` is found in the context buffer.
    ///
    /// If the `request_id` is not found in the context buffer, return `None`.
    /// If the `runtime_duration_ms` hasn't been seen, return `None`.
    ///
    pub fn on_platform_report(&mut self, request_id: &str, metrics: ReportMetrics, timestamp: i64) {
        // Set the report log metrics
        self.enhanced_metrics
            .set_report_log_metrics(&metrics, timestamp);

        if let Some(context) = self.context_buffer.get(request_id) {
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

    pub fn on_shutdown_event(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unable to poll clock, unrecoverable")
            .as_secs();
        self.enhanced_metrics
            .set_shutdown_metric(i64::try_from(now).expect("can't convert now to i64"));
    }

    /// If this method is called, it means that we are operating in a Universally Instrumented
    /// runtime. Therefore, we need to set the `tracer_detected` flag to `true`.
    ///
    pub fn on_universal_instrumentation_start(
        &mut self,
        headers: HashMap<String, String>,
        payload: Vec<u8>,
    ) -> Option<SpanContext> {
        self.tracer_detected = true;

        let payload_value = serde_json::from_slice::<Value>(&payload).unwrap_or_else(|_| json!({}));
        // Infer span
        // todo(jordan): `infer_span` needs to be called before `extract_span_context`,
        // this is not ideal, as they are separate operations and should not depend on each other.
        self.inferrer.infer_span(&payload_value, &self.aws_config);
        let span_context = self.extract_span_context(&headers, &payload_value);

        // If `Invoke` event happened first, process right away
        if let Some(request_id) = self
            .context_buffer
            .pair_universal_instrumentation_start_event(&headers, &payload)
        {
            self.process_on_universal_instrumentation_start(
                request_id,
                payload,
                span_context.clone(),
            );
        }

        span_context
    }

    fn process_on_universal_instrumentation_start(
        &mut self,
        request_id: String,
        payload: Vec<u8>,
        span_context: Option<SpanContext>,
    ) {
        let Some(context) = self.context_buffer.get_mut(&request_id) else {
            debug!("Cannot process on invocation start, no context for request_id: {request_id}");
            return;
        };

        let payload_value = serde_json::from_slice::<Value>(&payload).unwrap_or_else(|_| json!({}));
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

        context.extracted_span_context = span_context;

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

    fn extract_span_context(
        &self,
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
    pub fn on_universal_instrumentation_end(
        &mut self,
        headers: HashMap<String, String>,
        payload: Vec<u8>,
    ) {
        // If `PlatformRuntimeDone` event happened first, process
        if let Some(request_id) = self
            .context_buffer
            .pair_universal_instrumentation_end_event(&headers, &payload)
        {
            self.process_on_universal_instrumentation_end(request_id, headers, payload);
        }
    }

    fn process_on_universal_instrumentation_end(
        &mut self,
        request_id: String,
        headers: HashMap<String, String>,
        payload: Vec<u8>,
    ) {
        let Some(context) = self.context_buffer.get_mut(&request_id) else {
            debug!("Cannot process on invocation end, no context for request_id: {request_id}");
            return;
        };

        let payload_value = serde_json::from_slice::<Value>(&payload).unwrap_or_else(|_| json!({}));

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

            if let Some(priority_str) = headers.get(DATADOG_SAMPLING_PRIORITY_KEY) {
                if let Ok(priority) = priority_str.parse::<f64>() {
                    context
                        .invocation_span
                        .metrics
                        .insert(TAG_SAMPLING_PRIORITY.to_string(), priority);
                }
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
                m.to_string()
            });

            error_tags.insert(String::from("error.msg"), decoded_message);
        }

        if let Some(t) = r#type {
            let decoded_type = base64_to_string(t).unwrap_or_else(|_| {
                debug!("Error type header may not be encoded, setting as is");
                t.to_string()
            });

            error_tags.insert(String::from("error.type"), decoded_type);
        }

        if let Some(s) = stack {
            let decoded_stack = base64_to_string(s).unwrap_or_else(|e| {
                debug!("Failed to decode error stack: {e}");
                s.to_string()
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
    use base64::{engine::general_purpose::STANDARD, Engine};
    use dogstatsd::aggregator::Aggregator;
    use dogstatsd::metric::EMPTY_TAGS;

    fn setup() -> Processor {
        let aws_config = AwsConfig {
            region: "us-east-1".into(),
            aws_lwa_proxy_lambda_runtime_api: Some("***".into()),
            function_name: "test-function".into(),
            sandbox_init_time: Instant::now(),
            runtime_api: "***".into(),
            exec_wrapper: None,
        };

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

        let metrics_aggregator = Arc::new(Mutex::new(
            Aggregator::new(EMPTY_TAGS, 1024).expect("failed to create aggregator"),
        ));

        Processor::new(tags_provider, config, &aws_config, metrics_aggregator)
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

    #[test]
    fn test_process_on_universal_instrumentation_end_headers_with_sampling_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "999".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "1000".to_string());
        headers.insert(DATADOG_SAMPLING_PRIORITY_KEY.to_string(), "-1".to_string());

        let request_id = String::from("request_id");
        p.context_buffer.start_context(&request_id, Span::default());

        p.process_on_universal_instrumentation_end(request_id.clone(), headers, vec![]);

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

    #[test]
    fn test_process_on_universal_instrumentation_end_headers_with_invalid_priority() {
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

        p.process_on_universal_instrumentation_end(request_id.clone(), headers, vec![]);

        let context = p.context_buffer.get(&request_id).unwrap();

        assert!(!context
            .invocation_span
            .metrics
            .contains_key(TAG_SAMPLING_PRIORITY));
        assert_eq!(context.invocation_span.trace_id, 888);
        assert_eq!(context.invocation_span.parent_id, 999);
    }

    #[test]
    fn test_process_on_universal_instrumentation_end_headers_no_sampling_priority() {
        let mut p = setup();
        let mut headers = HashMap::new();

        headers.insert(DATADOG_TRACE_ID_KEY.to_string(), "111".to_string());
        headers.insert(DATADOG_PARENT_ID_KEY.to_string(), "222".to_string());

        let request_id = String::from("request_id");
        p.context_buffer.start_context(&request_id, Span::default());

        p.process_on_universal_instrumentation_end(request_id.clone(), headers, vec![]);

        let context = p.context_buffer.get(&request_id).unwrap();

        assert!(!context
            .invocation_span
            .metrics
            .contains_key(TAG_SAMPLING_PRIORITY));
        assert_eq!(context.invocation_span.trace_id, 111);
        assert_eq!(context.invocation_span.parent_id, 222);
    }

    #[test]
    fn test_process_on_invocation_end_tags_response_with_status_code() {
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
        p.process_on_universal_instrumentation_end(
            request_id.clone(),
            HashMap::new(),
            response.as_bytes().to_vec(),
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
}
