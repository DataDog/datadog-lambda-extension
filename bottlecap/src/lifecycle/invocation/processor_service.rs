use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use dogstatsd::aggregator_service::AggregatorHandle;
use libdd_trace_protobuf::pb::Span;
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::{
    config::{self, aws::AwsConfig},
    extension::telemetry::events::{
        InitType, ReportMetrics, RuntimeDoneMetrics, Status, TelemetrySpan,
    },
    lifecycle::invocation::{
        context::{Context, ReparentingInfo},
        processor::Processor,
    },
    tags::provider,
    traces::{
        context::SpanContext, propagation::DatadogCompositePropagator,
        trace_processor::SendingTraceProcessor,
    },
};

#[derive(Error, Debug)]
pub enum ProcessorError {
    #[error("Channel send error: {0}")]
    ChannelSend(String),
    #[error("Channel receive error: {0}")]
    ChannelReceive(String),
    #[error("Service is shutting down")]
    ServiceShutdown,
}

pub enum ProcessorCommand {
    InvokeEvent {
        request_id: String,
    },
    PlatformInitStart {
        time: DateTime<Utc>,
    },
    PlatformInitReport {
        init_type: InitType,
        duration_ms: f64,
        timestamp: i64,
    },
    PlatformRestoreStart {
        time: DateTime<Utc>,
    },
    PlatformRestoreReport {
        duration_ms: f64,
        timestamp: i64,
    },
    PlatformStart {
        request_id: String,
        time: DateTime<Utc>,
    },
    PlatformRuntimeDone {
        request_id: String,
        metrics: RuntimeDoneMetrics,
        status: Status,
        error_type: Option<String>,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
        timestamp: i64,
        response: oneshot::Sender<()>,
    },
    PlatformReport {
        request_id: String,
        metrics: ReportMetrics,
        timestamp: i64,
        status: Status,
        error_type: Option<String>,
        spans: Option<Vec<TelemetrySpan>>,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
        response: oneshot::Sender<()>,
    },
    UniversalInstrumentationStart {
        headers: HashMap<String, String>,
        payload_value: Value,
        request_id: Option<String>,
    },
    UniversalInstrumentationEnd {
        headers: HashMap<String, String>,
        payload_value: Value,
        request_id: Option<String>,
    },
    AddReparenting {
        request_id: String,
        span_id: u64,
        parent_id: u64,
    },
    GetReparentingInfo {
        response:
            oneshot::Sender<Result<std::collections::VecDeque<ReparentingInfo>, ProcessorError>>,
    },
    UpdateReparenting {
        reparenting_info: std::collections::VecDeque<ReparentingInfo>,
        response: oneshot::Sender<
            Result<Vec<crate::lifecycle::invocation::context::Context>, ProcessorError>,
        >,
    },
    SetColdStartSpanTraceId {
        trace_id: u64,
        response: oneshot::Sender<Result<Option<u64>, ProcessorError>>,
    },
    AddTracerSpan {
        span: Box<Span>,
    },
    OnOutOfMemoryError {
        timestamp: i64,
    },
    OnShutdownEvent,
    SendCtxSpans {
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
        context: Box<Context>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct InvocationProcessorHandle {
    sender: mpsc::Sender<ProcessorCommand>,
}

impl InvocationProcessorHandle {
    pub async fn on_invoke_event(
        &self,
        request_id: String,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::InvokeEvent { request_id })
            .await
    }

    pub async fn on_platform_init_start(
        &self,
        time: DateTime<Utc>,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::PlatformInitStart { time })
            .await
    }

    pub async fn on_platform_init_report(
        &self,
        init_type: InitType,
        duration_ms: f64,
        timestamp: i64,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::PlatformInitReport {
                init_type,
                duration_ms,
                timestamp,
            })
            .await
    }

    pub async fn on_platform_restore_start(
        &self,
        time: DateTime<Utc>,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::PlatformRestoreStart { time })
            .await
    }

    pub async fn on_platform_restore_report(
        &self,
        duration_ms: f64,
        timestamp: i64,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::PlatformRestoreReport {
                duration_ms,
                timestamp,
            })
            .await
    }

    pub async fn on_platform_start(
        &self,
        request_id: String,
        time: DateTime<Utc>,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::PlatformStart { request_id, time })
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn on_platform_runtime_done(
        &self,
        request_id: String,
        metrics: RuntimeDoneMetrics,
        status: Status,
        error_type: Option<String>,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
        timestamp: i64,
    ) -> Result<(), ProcessorError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(ProcessorCommand::PlatformRuntimeDone {
                request_id,
                metrics,
                status,
                error_type,
                tags_provider,
                trace_sender,
                timestamp,
                response: tx,
            })
            .await
            .map_err(|e| ProcessorError::ChannelSend(e.to_string()))?;
        rx.await
            .map_err(|e| ProcessorError::ChannelReceive(e.to_string()))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn on_platform_report(
        &self,
        request_id: &str,
        metrics: ReportMetrics,
        timestamp: i64,
        status: Status,
        error_type: &Option<String>,
        spans: &Option<Vec<TelemetrySpan>>,
        tags_provider: Arc<provider::Provider>,
        trace_sender: Arc<SendingTraceProcessor>,
    ) -> Result<(), ProcessorError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(ProcessorCommand::PlatformReport {
                request_id: request_id.to_string(),
                metrics,
                timestamp,
                status,
                error_type: error_type.clone(),
                spans: spans.clone(),
                tags_provider,
                trace_sender,
                response: tx,
            })
            .await
            .map_err(|e| ProcessorError::ChannelSend(e.to_string()))?;
        rx.await
            .map_err(|e| ProcessorError::ChannelReceive(e.to_string()))?;
        Ok(())
    }

    pub async fn on_universal_instrumentation_start(
        &self,
        headers: HashMap<String, String>,
        payload_value: Value,
        request_id: Option<String>,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::UniversalInstrumentationStart {
                headers,
                payload_value,
                request_id,
            })
            .await
    }

    pub async fn on_universal_instrumentation_end(
        &self,
        headers: HashMap<String, String>,
        payload_value: Value,
        request_id: Option<String>,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::UniversalInstrumentationEnd {
                headers,
                payload_value,
                request_id,
            })
            .await
    }

    pub async fn add_reparenting(
        &self,
        request_id: String,
        span_id: u64,
        parent_id: u64,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::AddReparenting {
                request_id,
                span_id,
                parent_id,
            })
            .await
    }

    pub async fn get_reparenting_info(
        &self,
    ) -> Result<std::collections::VecDeque<ReparentingInfo>, ProcessorError> {
        let (response_tx, response_rx) = oneshot::channel();

        self.sender
            .send(ProcessorCommand::GetReparentingInfo {
                response: response_tx,
            })
            .await
            .map_err(|e| ProcessorError::ChannelSend(e.to_string()))?;

        response_rx
            .await
            .map_err(|e| ProcessorError::ChannelReceive(e.to_string()))?
    }

    pub async fn update_reparenting(
        &self,
        reparenting_info: std::collections::VecDeque<ReparentingInfo>,
    ) -> Result<Vec<crate::lifecycle::invocation::context::Context>, ProcessorError> {
        let (response_tx, response_rx) = oneshot::channel();

        self.sender
            .send(ProcessorCommand::UpdateReparenting {
                reparenting_info,
                response: response_tx,
            })
            .await
            .map_err(|e| ProcessorError::ChannelSend(e.to_string()))?;

        response_rx
            .await
            .map_err(|e| ProcessorError::ChannelReceive(e.to_string()))?
    }

    #[must_use]
    pub fn extract_span_context(
        headers: &HashMap<String, String>,
        payload_value: &Value,
        propagator: Arc<DatadogCompositePropagator>,
    ) -> Option<SpanContext> {
        Processor::extract_span_context(headers, payload_value, propagator)
    }

    pub async fn set_cold_start_span_trace_id(
        &self,
        trace_id: u64,
    ) -> Result<Option<u64>, ProcessorError> {
        let (response_tx, response_rx) = oneshot::channel();

        self.sender
            .send(ProcessorCommand::SetColdStartSpanTraceId {
                trace_id,
                response: response_tx,
            })
            .await
            .map_err(|e| ProcessorError::ChannelSend(e.to_string()))?;

        response_rx
            .await
            .map_err(|e| ProcessorError::ChannelReceive(e.to_string()))?
    }

    pub async fn add_tracer_span(
        &self,
        span: Span,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::AddTracerSpan {
                span: Box::new(span),
            })
            .await
    }

    pub async fn on_out_of_memory_error(
        &self,
        timestamp: i64,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::OnOutOfMemoryError { timestamp })
            .await
    }

    pub async fn on_shutdown_event(&self) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender.send(ProcessorCommand::OnShutdownEvent).await
    }

    pub async fn send_ctx_spans(
        &self,
        tags_provider: &Arc<provider::Provider>,
        trace_sender: &Arc<SendingTraceProcessor>,
        context: Context,
    ) -> Result<(), mpsc::error::SendError<ProcessorCommand>> {
        self.sender
            .send(ProcessorCommand::SendCtxSpans {
                tags_provider: Arc::clone(tags_provider),
                trace_sender: Arc::clone(trace_sender),
                context: Box::new(context),
            })
            .await
    }

    pub async fn shutdown(&self) -> Result<(), ProcessorError> {
        self.sender
            .send(ProcessorCommand::Shutdown)
            .await
            .map_err(|e| ProcessorError::ChannelSend(e.to_string()))?;

        Ok(())
    }
}
pub struct InvocationProcessorService {
    processor: Processor,
    receiver: mpsc::Receiver<ProcessorCommand>,
}

impl InvocationProcessorService {
    #[must_use]
    pub fn new(
        tags_provider: Arc<provider::Provider>,
        config: Arc<config::Config>,
        aws_config: Arc<AwsConfig>,
        metrics_aggregator_handle: AggregatorHandle,
        propagator: Arc<DatadogCompositePropagator>,
    ) -> (InvocationProcessorHandle, Self) {
        let (sender, receiver) = mpsc::channel(1000);

        let processor = Processor::new(
            tags_provider,
            config,
            aws_config,
            metrics_aggregator_handle,
            propagator,
        );

        let handle = InvocationProcessorHandle { sender };
        let service = Self {
            processor,
            receiver,
        };

        (handle, service)
    }

    #[allow(clippy::too_many_lines)]
    pub async fn run(mut self) {
        debug!("InvocationProcessorService starting");

        while let Some(command) = self.receiver.recv().await {
            match command {
                ProcessorCommand::InvokeEvent { request_id } => {
                    self.processor.on_invoke_event(request_id);
                }
                ProcessorCommand::PlatformInitStart { time } => {
                    self.processor.on_platform_init_start(time);
                }
                ProcessorCommand::PlatformInitReport {
                    init_type,
                    duration_ms,
                    timestamp,
                } => {
                    self.processor
                        .on_platform_init_report(init_type, duration_ms, timestamp);
                }
                ProcessorCommand::PlatformRestoreStart { time } => {
                    self.processor.on_platform_restore_start(time);
                }
                ProcessorCommand::PlatformRestoreReport {
                    duration_ms,
                    timestamp,
                } => {
                    self.processor
                        .on_platform_restore_report(duration_ms, timestamp);
                }
                ProcessorCommand::PlatformStart { request_id, time } => {
                    self.processor.on_platform_start(request_id, time);
                }
                ProcessorCommand::PlatformRuntimeDone {
                    request_id,
                    metrics,
                    status,
                    error_type,
                    tags_provider,
                    trace_sender,
                    timestamp,
                    response,
                } => {
                    self.processor
                        .on_platform_runtime_done(
                            &request_id,
                            metrics,
                            status,
                            error_type,
                            tags_provider,
                            trace_sender,
                            timestamp,
                        )
                        .await;
                    let _ = response.send(());
                }
                ProcessorCommand::PlatformReport {
                    request_id,
                    metrics,
                    timestamp,
                    status,
                    error_type,
                    spans,
                    tags_provider,
                    trace_sender,
                    response,
                } => {
                    self.processor
                        .on_platform_report(
                            &request_id,
                            metrics,
                            timestamp,
                            status,
                            error_type,
                            spans,
                            tags_provider,
                            trace_sender,
                        )
                        .await;

                    // The necessity of having response.send():
                    // Without it, the caller at line 187-188 (rx.await) would block forever waiting for a response
                    //  - The async task would never complete
                    //  - The entire extension could hang during shutdown
                    // this change also mirrors the behavior of handling PlatformRuntimeDone in OD mode
                    let _ = response.send(());
                }
                ProcessorCommand::UniversalInstrumentationStart {
                    headers,
                    payload_value,
                    request_id,
                } => {
                    self.processor.on_universal_instrumentation_start(
                        headers,
                        payload_value,
                        request_id,
                    );
                }
                ProcessorCommand::UniversalInstrumentationEnd {
                    headers,
                    payload_value,
                    request_id,
                } => {
                    self.processor.on_universal_instrumentation_end(
                        headers,
                        payload_value,
                        request_id,
                    );
                }
                ProcessorCommand::AddReparenting {
                    request_id,
                    span_id,
                    parent_id,
                } => {
                    self.processor
                        .add_reparenting(request_id, span_id, parent_id);
                }
                ProcessorCommand::GetReparentingInfo { response } => {
                    let result = Ok(self.processor.get_reparenting_info());
                    let _ = response.send(result);
                }
                ProcessorCommand::UpdateReparenting {
                    reparenting_info,
                    response,
                } => {
                    let result = Ok(self.processor.update_reparenting(reparenting_info));
                    let _ = response.send(result);
                }
                ProcessorCommand::SetColdStartSpanTraceId { trace_id, response } => {
                    let result = Ok(self.processor.set_cold_start_span_trace_id(trace_id));
                    let _ = response.send(result);
                }
                ProcessorCommand::AddTracerSpan { span } => {
                    self.processor.add_tracer_span(&span);
                }
                ProcessorCommand::OnOutOfMemoryError { timestamp } => {
                    self.processor.on_out_of_memory_error(timestamp);
                }
                ProcessorCommand::OnShutdownEvent => {
                    self.processor.on_shutdown_event();
                }
                ProcessorCommand::SendCtxSpans {
                    tags_provider,
                    trace_sender,
                    context,
                } => {
                    self.processor
                        .send_ctx_spans(&tags_provider, &trace_sender, *context)
                        .await;
                }
                ProcessorCommand::Shutdown => {
                    debug!("InvocationProcessorService shutting down");
                    break;
                }
            }
        }

        debug!("InvocationProcessorService stopped");
    }
}
