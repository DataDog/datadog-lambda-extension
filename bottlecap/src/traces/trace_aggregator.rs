use libdd_trace_utils::send_data::SendDataBuilder;
use libdd_trace_utils::trace_utils::TracerHeaderTags;
use std::collections::VecDeque;

/// Maximum content size per payload uncompressed in bytes,
/// that the Datadog Trace API accepts. The value is 3.2 MB.
///
/// <https://github.com/DataDog/datadog-agent/blob/9d57c10a9eeb3916e661d35dbd23c6e36395a99d/pkg/trace/writer/trace.go#L27-L31>
pub const MAX_CONTENT_SIZE_BYTES: usize = 3_200_000;

/// Owned version of `TracerHeaderTags<'a>` so it can be stored across async
/// boundaries without lifetime issues.
pub struct OwnedTracerHeaderTags {
    pub lang: String,
    pub lang_version: String,
    pub lang_interpreter: String,
    pub lang_vendor: String,
    pub tracer_version: String,
    pub container_id: String,
    pub client_computed_top_level: bool,
    pub client_computed_stats: bool,
    pub dropped_p0_traces: usize,
    pub dropped_p0_spans: usize,
}

impl From<TracerHeaderTags<'_>> for OwnedTracerHeaderTags {
    fn from(tags: TracerHeaderTags<'_>) -> Self {
        Self {
            lang: tags.lang.to_string(),
            lang_version: tags.lang_version.to_string(),
            lang_interpreter: tags.lang_interpreter.to_string(),
            lang_vendor: tags.lang_vendor.to_string(),
            tracer_version: tags.tracer_version.to_string(),
            container_id: tags.container_id.to_string(),
            client_computed_top_level: tags.client_computed_top_level,
            client_computed_stats: tags.client_computed_stats,
            dropped_p0_traces: tags.dropped_p0_traces,
            dropped_p0_spans: tags.dropped_p0_spans,
        }
    }
}

impl OwnedTracerHeaderTags {
    #[must_use]
    pub fn to_tracer_header_tags(&self) -> TracerHeaderTags<'_> {
        TracerHeaderTags {
            lang: &self.lang,
            lang_version: &self.lang_version,
            lang_interpreter: &self.lang_interpreter,
            lang_vendor: &self.lang_vendor,
            tracer_version: &self.tracer_version,
            container_id: &self.container_id,
            client_computed_top_level: self.client_computed_top_level,
            client_computed_stats: self.client_computed_stats,
            dropped_p0_traces: self.dropped_p0_traces,
            dropped_p0_spans: self.dropped_p0_spans,
        }
    }
}

// Bundle SendDataBuilder with payload size because SendDataBuilder doesn't
// expose a getter for the size
pub struct SendDataBuilderInfo {
    pub builder: SendDataBuilder,
    pub size: usize,
    pub header_tags: OwnedTracerHeaderTags,
}

impl SendDataBuilderInfo {
    pub fn new(builder: SendDataBuilder, size: usize, header_tags: OwnedTracerHeaderTags) -> Self {
        Self {
            builder,
            size,
            header_tags,
        }
    }
}

/// Takes in individual trace payloads and aggregates them into batches to be flushed to Datadog.
#[allow(clippy::module_name_repetitions)]
pub struct TraceAggregator {
    queue: VecDeque<SendDataBuilderInfo>,
    max_content_size_bytes: usize,
    buffer: Vec<SendDataBuilderInfo>,
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
    pub fn get_batch(&mut self) -> Vec<SendDataBuilderInfo> {
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
                self.buffer.push(payload_info);
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

    fn make_header_tags() -> TracerHeaderTags<'static> {
        TracerHeaderTags {
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
        }
    }

    fn make_builder_info(size: usize) -> SendDataBuilderInfo {
        let tracer_header_tags = make_header_tags();
        let builder = SendDataBuilder::new(
            size,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags.clone(),
            &Endpoint::from_slice("localhost"),
        );
        SendDataBuilderInfo::new(
            builder,
            size,
            OwnedTracerHeaderTags::from(tracer_header_tags),
        )
    }

    #[test]
    fn test_add() {
        let mut aggregator = TraceAggregator::default();
        let size = 1;

        aggregator.add(make_builder_info(size));
        assert_eq!(aggregator.queue.len(), 1);
    }

    #[test]
    fn test_get_batch() {
        let mut aggregator = TraceAggregator::default();
        let size = 1;

        aggregator.add(make_builder_info(size));
        assert_eq!(aggregator.queue.len(), 1);
        let batch = aggregator.get_batch();
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn test_get_batch_full_entries() {
        let mut aggregator = TraceAggregator::new(2);
        let size = 1;

        // Add 3 payloads
        aggregator.add(make_builder_info(size));
        aggregator.add(make_builder_info(size));
        aggregator.add(make_builder_info(size));

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
