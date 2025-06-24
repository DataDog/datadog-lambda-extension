use crate::config::Config;
use crate::tags::provider::Provider;
use crate::traces::trace_processor::{ServerlessTraceProcessor, TraceProcessor};
use datadog_trace_utils::send_data::SendData;
use datadog_trace_utils::tracer_header_tags::TracerHeaderTags;
use std::collections::VecDeque;
use std::sync::Arc;

/// Maximum content size per payload uncompressed in bytes,
/// that the Datadog Trace API accepts. The value is 3.2 MB.
///
/// <https://github.com/DataDog/datadog-agent/blob/9d57c10a9eeb3916e661d35dbd23c6e36395a99d/pkg/trace/writer/trace.go#L27-L31>
pub const MAX_CONTENT_SIZE_BYTES: usize = (32 * 1_024 * 1_024) / 10;

/// Raw trace data to be processed
#[derive(Clone)]
pub struct RawTraceData {
    pub traces: Vec<Vec<datadog_trace_protobuf::pb::Span>>,
    pub body_size: usize,
}

#[allow(clippy::module_name_repetitions)]
pub struct TraceAggregator {
    queue: VecDeque<RawTraceData>,
    max_content_size_bytes: usize,
    buffer: Vec<RawTraceData>,
}

impl TraceAggregator {
    #[allow(clippy::must_use_candidate)]
    pub fn new() -> Self {
        TraceAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes: MAX_CONTENT_SIZE_BYTES,
            buffer: Vec::new(),
        }
    }

    pub fn add(&mut self, p: RawTraceData) {
        self.queue.push_back(p);
    }

    pub fn get_batch(
        &mut self,
        trace_processor: &ServerlessTraceProcessor,
        config: &Arc<Config>,
        tags_provider: &Arc<Provider>,
    ) -> Vec<SendData> {
        let mut batch_size = 0;
        let mut send_data_batch = Vec::new();

        // Collect raw trace data up to batch size
        while batch_size < self.max_content_size_bytes {
            if let Some(raw_data) = self.queue.pop_front() {
                if batch_size + raw_data.body_size > self.max_content_size_bytes
                    && !self.buffer.is_empty()
                {
                    self.queue.push_front(raw_data);
                    break;
                }
                batch_size += raw_data.body_size;
                self.buffer.push(raw_data);
            } else {
                break;
            }
        }

        // Process each raw trace data into SendData
        for raw_data in std::mem::take(&mut self.buffer) {
            // Create default header tags since we don't have the original ones
            let header_tags = TracerHeaderTags {
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

            let send_data = trace_processor.process_traces(
                config.clone(),
                tags_provider.clone(),
                header_tags,
                raw_data.traces,
                raw_data.body_size,
                None,
            );
            send_data_batch.push(send_data);
        }

        send_data_batch
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use datadog_trace_utils::{
        trace_utils::TracerHeaderTags, tracer_payload::TracerPayloadCollection,
    };
    use ddcommon::Endpoint;

    use super::*;

    #[test]
    fn test_add() {
        let mut aggregator = TraceAggregator::default();
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
        let payload = SendData::new(
            1,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        );

        aggregator.add(payload.clone());
        assert_eq!(aggregator.queue.len(), 1);
        assert_eq!(aggregator.queue[0].is_empty(), payload.is_empty());
    }

    #[test]
    fn test_get_batch() {
        let mut aggregator = TraceAggregator::default();
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
        let payload = SendData::new(
            1,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        );

        aggregator.add(payload.clone());
        assert_eq!(aggregator.queue.len(), 1);
        let batch = aggregator.get_batch();
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn test_get_batch_full_entries() {
        let mut aggregator = TraceAggregator::new(2);
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
        let payload = SendData::new(
            1,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        );

        // Add 3 payloads
        aggregator.add(payload.clone());
        aggregator.add(payload.clone());
        aggregator.add(payload.clone());

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
