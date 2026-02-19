use libdd_trace_utils::send_data::SendDataBuilder;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error};

use crate::traces::trace_aggregator::{
    MAX_CONTENT_SIZE_BYTES, SendDataBuilderInfo, TraceAggregator,
};

pub enum AggregatorCommand {
    InsertPayload(Box<SendDataBuilderInfo>),
    GetBatches(oneshot::Sender<Vec<Vec<SendDataBuilder>>>),
    Clear,
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct AggregatorHandle {
    tx: mpsc::UnboundedSender<AggregatorCommand>,
}

impl AggregatorHandle {
    pub fn insert_payload(
        &self,
        payload_info: SendDataBuilderInfo,
    ) -> Result<(), mpsc::error::SendError<AggregatorCommand>> {
        self.tx
            .send(AggregatorCommand::InsertPayload(Box::new(payload_info)))
    }

    pub async fn get_batches(&self) -> Result<Vec<Vec<SendDataBuilder>>, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(AggregatorCommand::GetBatches(response_tx))
            .map_err(|e| format!("Failed to send flush command: {e}"))?;

        response_rx
            .await
            .map_err(|e| format!("Failed to receive flush response: {e}"))
    }

    pub fn clear(&self) -> Result<(), mpsc::error::SendError<AggregatorCommand>> {
        self.tx.send(AggregatorCommand::Clear)
    }

    pub fn shutdown(&self) -> Result<(), mpsc::error::SendError<AggregatorCommand>> {
        self.tx.send(AggregatorCommand::Shutdown)
    }
}

pub struct AggregatorService {
    aggregator: TraceAggregator,
    rx: mpsc::UnboundedReceiver<AggregatorCommand>,
}

impl AggregatorService {
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> (Self, AggregatorHandle) {
        Self::new(MAX_CONTENT_SIZE_BYTES)
    }

    #[must_use]
    pub fn new(max_content_size_bytes: usize) -> (Self, AggregatorHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let aggregator = TraceAggregator::new(max_content_size_bytes);

        let service = Self { aggregator, rx };
        let handle = AggregatorHandle { tx };

        (service, handle)
    }

    pub async fn run(mut self) {
        debug!("Trace aggregator service started");

        while let Some(command) = self.rx.recv().await {
            match command {
                AggregatorCommand::InsertPayload(payload_info) => {
                    self.aggregator.add(*payload_info);
                }
                AggregatorCommand::GetBatches(response_tx) => {
                    let mut batches = Vec::new();
                    let mut current_batch = self.aggregator.get_batch();
                    while !current_batch.is_empty() {
                        batches.push(current_batch);
                        current_batch = self.aggregator.get_batch();
                    }
                    if response_tx.send(batches).is_err() {
                        error!("Failed to send trace flush response - receiver dropped");
                    }
                }
                AggregatorCommand::Clear => {
                    self.aggregator.clear();
                }
                AggregatorCommand::Shutdown => {
                    debug!("Trace aggregator service shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use libdd_common::Endpoint;
    use libdd_trace_utils::{
        trace_utils::TracerHeaderTags, tracer_payload::TracerPayloadCollection,
    };

    #[tokio::test]
    async fn test_aggregator_service_insert_and_flush() {
        let (service, handle) = AggregatorService::default();

        let service_handle = tokio::spawn(async move {
            service.run().await;
        });

        let tracer_header_tags = TracerHeaderTags {
            lang: "lang",
            lang_version: "lang_version",
            lang_interpreter: "lang_interpreter",
            lang_vendor: "lang_vendor",
            tracer_version: "tracer_version",
            container_id: "container_id",
            client_computed_top_level: true,
            client_computed_stats: true,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };
        let size = 1;
        let payload = SendDataBuilder::new(
            size,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        );

        handle
            .insert_payload(SendDataBuilderInfo::new(payload, size))
            .unwrap();

        let batches = handle.get_batches().await.unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);

        handle
            .shutdown()
            .expect("Failed to shutdown aggregator service");
        service_handle
            .await
            .expect("Aggregator service task failed");
    }
}
