use libdd_trace_utils::send_data::SendDataBuilder;
use std::collections::VecDeque;

/// Maximum content size per payload uncompressed in bytes,
/// that the Datadog Trace API accepts. The value is 3.2 MB.
///
/// <https://github.com/DataDog/datadog-agent/blob/9d57c10a9eeb3916e661d35dbd23c6e36395a99d/pkg/trace/writer/trace.go#L27-L31>
pub const MAX_CONTENT_SIZE_BYTES: usize = 3_200_000;

// Bundle SendDataBuilder with payload size because SendDataBuilder doesn't
// expose a getter for the size
pub struct SendDataBuilderInfo {
    pub builder: SendDataBuilder,
    pub size: usize,
}

impl SendDataBuilderInfo {
    pub fn new(builder: SendDataBuilder, size: usize) -> Self {
        Self { builder, size }
    }
}

/// Takes in individual trace payloads and aggregates them into batches to be flushed to Datadog.
#[allow(clippy::module_name_repetitions)]
pub struct TraceAggregator {
    queue: VecDeque<SendDataBuilderInfo>,
    max_content_size_bytes: usize,
    buffer: Vec<SendDataBuilder>,
}

impl Default for TraceAggregator {
    fn default() -> Self {
        TraceAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes: MAX_CONTENT_SIZE_BYTES,
            buffer: Vec::new(),
        }
    }
}

impl TraceAggregator {
    #[allow(dead_code)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(max_content_size_bytes: usize) -> Self {
        TraceAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes,
            buffer: Vec::new(),
        }
    }

    /// Takes in an individual trace payload.
    pub fn add(&mut self, payload_info: SendDataBuilderInfo) {
        self.queue.push_back(payload_info);
    }

    /// Returns a batch of trace payloads, subject to the max content size.
    pub fn get_batch(&mut self) -> Vec<SendDataBuilder> {
        let mut batch_size = 0;

        // Fill the batch
        while batch_size < self.max_content_size_bytes {
            if let Some(payload_info) = self.queue.pop_front() {
                // TODO(duncanista): revisit if this is bigger than limit
                let payload_size = payload_info.size;

                // Put stats back in the queue
                if batch_size + payload_size > self.max_content_size_bytes {
                    self.queue.push_front(payload_info);
                    break;
                }
                batch_size += payload_size;
                self.buffer.push(payload_info.builder);
            } else {
                break;
            }
        }

        std::mem::take(&mut self.buffer)
    }

    /// Flush the queue.
    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use libdd_common::Endpoint;
    use libdd_trace_utils::{
        trace_utils::TracerHeaderTags, tracer_payload::TracerPayloadCollection,
    };

    use super::*;

    fn make_builder(size: usize) -> SendDataBuilder {
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
        SendDataBuilder::new(
            size,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        )
    }

    #[test]
    fn test_add() {
        let mut aggregator = TraceAggregator::default();
        let size = 1;

        aggregator.add(SendDataBuilderInfo::new(make_builder(size), size));
        assert_eq!(aggregator.queue.len(), 1);
    }

    #[test]
    fn test_get_batch() {
        let mut aggregator = TraceAggregator::default();
        let size = 1;

        aggregator.add(SendDataBuilderInfo::new(make_builder(size), size));
        assert_eq!(aggregator.queue.len(), 1);
        let batch = aggregator.get_batch();
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn test_get_batch_full_entries() {
        let mut aggregator = TraceAggregator::new(2);
        let size = 1;

        // Add 3 payloads
        aggregator.add(SendDataBuilderInfo::new(make_builder(size), size));
        aggregator.add(SendDataBuilderInfo::new(make_builder(size), size));
        aggregator.add(SendDataBuilderInfo::new(make_builder(size), size));

        // The batch should only contain the first 2 payloads
        let first_batch = aggregator.get_batch();
        assert_eq!(first_batch.len(), 2);
        assert_eq!(aggregator.queue.len(), 1);

        // The second batch should only contain the last log
        let second_batch = aggregator.get_batch();
        assert_eq!(second_batch.len(), 1);
        assert_eq!(aggregator.queue.len(), 0);
    }
}
