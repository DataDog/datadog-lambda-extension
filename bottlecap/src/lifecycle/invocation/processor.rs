use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use datadog_trace_protobuf::pb::Span;
use datadog_trace_utils::{send_data::SendData, tracer_header_tags};
use tokio::sync::mpsc::Sender;
use tracing::debug;

use crate::{
    config::{self, AwsConfig},
    lifecycle::invocation::{context::ContextBuffer, span_inferrer::SpanInferrer},
    tags::provider,
    traces::trace_processor,
};

pub const MS_TO_NS: f64 = 1_000_000.0;

pub struct Processor {
    pub context_buffer: ContextBuffer,
    inferrer: SpanInferrer,
    pub span: Span,
    aws_config: AwsConfig,
    tracer_detected: bool,
}

impl Processor {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
        aws_config: &AwsConfig,
    ) -> Self {
        let service = config.service.clone().unwrap_or("aws.lambda".to_string());
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or("aws_lambda".to_string());

        Processor {
            context_buffer: ContextBuffer::default(),
            inferrer: SpanInferrer::default(),
            span: Span {
                service,
                name: "aws.lambda".to_string(),
                resource,
                trace_id: 0,  // set later
                span_id: 0,   // maybe set later?
                parent_id: 0, // set later
                start: 0,     // set later
                duration: 0,  // set later
                error: 0,
                meta: HashMap::new(),
                metrics: HashMap::new(),
                r#type: "serverless".to_string(),
                meta_struct: HashMap::new(),
                span_links: Vec::new(),
            },
            aws_config: aws_config.clone(),
            tracer_detected: false,
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
        self.context_buffer.add_start_time(&request_id, start_time);
        self.span.start = start_time;
    }

    #[allow(clippy::cast_possible_truncation)]
    pub async fn on_platform_runtime_done(
        &mut self,
        request_id: &String,
        duration_ms: f64,
        config: Arc<config::Config>,
        tags_provider: Arc<provider::Provider>,
        trace_processor: Arc<dyn trace_processor::TraceProcessor + Send + Sync>,
        trace_agent_tx: Sender<SendData>,
    ) {
        self.context_buffer
            .add_runtime_duration(request_id, duration_ms);

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
            // - trigger tags (from inferred spans)
            // - metrics tags (for asm)
        }

        self.inferrer.complete_inferred_span(&self.span);

        if self.tracer_detected {
            let mut body_size = std::mem::size_of_val(&self.span);
            let mut traces = vec![self.span.clone()];
            if let Some(inferred_span) = self.inferrer.get_inferred_span() {
                body_size += std::mem::size_of_val(inferred_span);
                traces.push(inferred_span.clone());
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
    pub fn on_platform_report(&mut self, request_id: &String, duration_ms: f64) -> Option<f64> {
        if let Some(context) = self.context_buffer.remove(request_id) {
            if context.runtime_duration_ms == 0.0 {
                return None;
            }

            return Some(duration_ms - context.runtime_duration_ms);
        }

        None
    }

    /// If this method is called, it means that we are operating in a Universally Instrumented
    /// runtime. Therefore, we need to set the `tracer_detected` flag to `true`.
    ///
    pub fn on_invocation_start(&mut self, payload: Vec<u8>) {
        self.tracer_detected = true;

        // Reset trace context
        self.span.trace_id = 0;
        self.span.parent_id = 0;
        self.span.span_id = 0;

        self.inferrer.infer_span(&payload, &self.aws_config);

        if let Some(inferred_span) = self.inferrer.get_inferred_span() {
            self.span.parent_id = inferred_span.span_id;
        }
    }

    /// Given trace context information, set it to the current span.
    ///
    pub fn on_invocation_end(
        &mut self,
        trace_id: u64,
        span_id: u64,
        parent_id: u64,
        status_code: Option<String>,
    ) {
        self.span.trace_id = trace_id;
        self.span.span_id = span_id;

        if self.inferrer.get_inferred_span().is_some() {
            self.inferrer.set_parent_id(parent_id);
            if let Some(status_code) = status_code {
                self.inferrer.set_status_code(status_code);
            }
        } else {
            self.span.parent_id = parent_id;
        }
    }
}