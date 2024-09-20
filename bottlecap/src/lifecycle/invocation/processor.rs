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
    config, lifecycle::invocation::context::ContextBuffer, tags::provider, traces::trace_processor,
};

pub const MS_TO_NS: f64 = 1_000_000.0;

pub struct Processor {
    pub context_buffer: ContextBuffer,
    pub current_request_id: String,
    pub span: Span,
    lambda_library_detected: bool,
}

impl Processor {
    #[must_use]
    pub fn new(tags_provider: Arc<provider::Provider>, config: Arc<config::Config>) -> Self {
        let service = config.service.clone().unwrap_or("aws.lambda".to_string());
        let resource = tags_provider
            .get_canonical_resource_name()
            .unwrap_or("aws_lambda".to_string());

        Processor {
            context_buffer: ContextBuffer::default(),
            current_request_id: String::new(),
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
            lambda_library_detected: true,
        }
    }

    pub fn on_platform_start(&mut self, request_id: String, time: DateTime<Utc>) {
        self.current_request_id.clone_from(&request_id);
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

        if let Some(context) = self.context_buffer.get(&self.current_request_id) {
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

        if !self.lambda_library_detected {
            let span_size = std::mem::size_of_val(&self.span);

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
                vec![vec![self.span.clone()]],
                span_size,
            );

            match trace_agent_tx.send(send_data).await {
                Ok(()) => println!("[usm] Sent span to agent"),
                Err(e) => println!("[usm] Failed to send span to agent: {e}"),
            }
        }
    }

    pub fn on_platform_report(&mut self, request_id: &String, duration_ms: f64) -> Option<f64> {
        debug!("[usm] on_platform_report");
        if let Some(context) = self.context_buffer.remove(request_id) {
            if context.runtime_duration_ms == 0.0 {
                return None;
            }

            return Some(duration_ms - context.runtime_duration_ms);
        }

        None
    }

    pub fn on_invocation_start(&mut self) {
        debug!("[usm] on_invocation_start");
        self.lambda_library_detected = false;
    }

    pub fn on_invocation_end(&mut self, trace_id: u64, span_id: u64, parent_id: u64) {
        debug!("[usm] on_invocation_end");

        let span = &mut self.span;
        span.trace_id = trace_id;
        span.span_id = span_id;
        span.parent_id = parent_id;
    }
}
