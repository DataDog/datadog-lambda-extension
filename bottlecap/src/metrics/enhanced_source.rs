//! A custom Saluki `Source` component for injecting enhanced Lambda metrics
//! into the Saluki pipeline programmatically.
//!
//! Enhanced metrics (invocations, duration, memory, CPU, etc.) are generated
//! by the Lambda extension itself — not received via DogStatsD UDP. This source
//! wraps a channel so that bottlecap code can send Saluki `Metric` objects
//! directly into the aggregation pipeline.

use std::sync::Mutex;

use async_trait::async_trait;
use memory_accounting::{MemoryBounds, MemoryBoundsBuilder};
use saluki_core::{
    components::{sources::*, ComponentContext},
    data_model::event::{metric::Metric, Event, EventType},
    topology::{EventsBuffer, OutputDefinition},
};
use saluki_error::GenericError;
use tokio::sync::mpsc;
use tracing::debug;

/// Handle for sending enhanced metrics into the Saluki pipeline.
#[derive(Clone, Debug)]
pub struct EnhancedMetricsHandle {
    tx: mpsc::UnboundedSender<Metric>,
}

impl EnhancedMetricsHandle {
    /// Send a single metric into the pipeline.
    pub fn send(&self, metric: Metric) {
        let _ = self.tx.send(metric);
    }

    /// Send a batch of metrics into the pipeline.
    pub fn send_batch(&self, metrics: Vec<Metric>) {
        for metric in metrics {
            let _ = self.tx.send(metric);
        }
    }
}

/// Configuration / builder for the enhanced metrics source.
/// Call `new()` to create both the builder and its handle.
pub struct EnhancedMetricsSourceBuilder {
    rx: Mutex<Option<mpsc::UnboundedReceiver<Metric>>>,
}

impl std::fmt::Debug for EnhancedMetricsSourceBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnhancedMetricsSourceBuilder").finish()
    }
}

impl EnhancedMetricsSourceBuilder {
    /// Create a new builder and its associated handle.
    pub fn new() -> (Self, EnhancedMetricsHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let builder = Self {
            rx: Mutex::new(Some(rx)),
        };
        let handle = EnhancedMetricsHandle { tx };
        (builder, handle)
    }
}

impl MemoryBounds for EnhancedMetricsSourceBuilder {
    fn specify_bounds(&self, builder: &mut MemoryBoundsBuilder) {
        builder
            .minimum()
            .with_single_value::<Self>("component struct");
    }
}

#[async_trait]
impl SourceBuilder for EnhancedMetricsSourceBuilder {
    fn outputs(&self) -> &[OutputDefinition<EventType>] {
        static OUTPUTS: &[OutputDefinition<EventType>] =
            &[OutputDefinition::default_output(EventType::Metric)];
        OUTPUTS
    }

    async fn build(
        &self,
        _context: ComponentContext,
    ) -> Result<Box<dyn Source + Send>, GenericError> {
        let rx = self
            .rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| saluki_error::generic_error!("EnhancedMetricsSource already built"))?;

        Ok(Box::new(EnhancedMetricsSource { rx }))
    }
}

/// The source component that receives metrics from the handle and
/// dispatches them into the Saluki topology.
struct EnhancedMetricsSource {
    rx: mpsc::UnboundedReceiver<Metric>,
}

#[async_trait]
impl Source for EnhancedMetricsSource {
    async fn run(
        mut self: Box<Self>,
        mut context: SourceContext,
    ) -> Result<(), GenericError> {
        let mut health = context.take_health_handle();
        let mut shutdown = context.take_shutdown_handle();

        health.mark_ready();
        debug!("Enhanced metrics source started.");

        let mut buffer = EventsBuffer::default();

        loop {
            tokio::select! {
                _ = health.live() => continue,
                _ = &mut shutdown => {
                    // Drain remaining metrics before shutting down
                    while let Ok(metric) = self.rx.try_recv() {
                        if buffer.try_push(Event::Metric(metric)).is_some() {
                            context.dispatcher().dispatch(buffer).await?;
                            buffer = EventsBuffer::default();
                        }
                    }
                    if !buffer.is_empty() {
                        context.dispatcher().dispatch(buffer).await?;
                    }
                    break;
                }
                maybe_metric = self.rx.recv() => {
                    match maybe_metric {
                        Some(metric) => {
                            if buffer.try_push(Event::Metric(metric)).is_some() {
                                // Buffer full, dispatch and start a new one
                                context.dispatcher().dispatch(buffer).await?;
                                buffer = EventsBuffer::default();
                            }
                        }
                        None => {
                            // Channel closed, flush remaining and exit
                            if !buffer.is_empty() {
                                context.dispatcher().dispatch(buffer).await?;
                            }
                            break;
                        }
                    }
                }
            }
        }

        debug!("Enhanced metrics source stopped.");
        Ok(())
    }
}
