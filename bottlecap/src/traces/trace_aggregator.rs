use prost::Message;
use datadog_trace_utils::send_data::SendData;
use std::collections::VecDeque;
use datadog_trace_protobuf::pb::ClientStatsPayload;

/// Maximum number of entries in a stat payload.
///
/// <https://github.com/DataDog/datadog-agent/blob/996dd54337908a6511948fabd2a41420ba919a8b/pkg/trace/writer/stats.go#L35-L41>
// const MAX_BATCH_ENTRIES_SIZE: usize = 4000;

/// Aproximate size an entry in a stat payload occupies
///
/// <https://github.com/DataDog/datadog-agent/blob/996dd54337908a6511948fabd2a41420ba919a8b/pkg/trace/writer/stats.go#L33-L35>
// const MAX_ENTRY_SIZE_BYTES: usize = 375;

/// Maximum content size per payload in compressed bytes,
///
/// <https://github.com/DataDog/datadog-agent/blob/996dd54337908a6511948fabd2a41420ba919a8b/pkg/trace/writer/stats.go#L35-L41>
pub const MAX_CONTENT_SIZE_BYTES_CPS: usize = 3 * 1024 * 1024; // ~3MB

/// Maximum content size per payload uncompressed in bytes,
/// that the Datadog Trace API accepts. The value is 3.2 MB.
///
/// <https://github.com/DataDog/datadog-agent/blob/9d57c10a9eeb3916e661d35dbd23c6e36395a99d/pkg/trace/writer/trace.go#L27-L31>
pub const MAX_CONTENT_SIZE_BYTES_SD: usize = (32 * 1_024 * 1_024) / 10;

#[allow(clippy::module_name_repetitions)]
pub struct BatchAggregator {
    queue: VecDeque<BatchData>,
    max_content_size_bytes: usize,
    buffer: Vec<BatchData>,
}

impl BatchAggregator {
    #[allow(dead_code)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(max_content_size_bytes: usize) -> Self {
        BatchAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes,
            buffer: Vec::with_capacity(max_content_size_bytes),
        }
    }

    pub fn add(&mut self, p: BatchData) {
        self.queue.push_back(p);
    }
    pub fn get_batch(&mut self) -> Vec<BatchData> {
        let mut batch_size = 0;

        // Fill the batch
        while batch_size < self.max_content_size_bytes {
            if let Some(payload) = self.queue.pop_front() {
                // TODO(duncanista): revisit if this is bigger than limit
                let payload_size = payload.len();

                // Put stats back in the queue
                if batch_size + payload_size > self.max_content_size_bytes {
                    self.queue.push_front(payload);
                    break;
                }
                batch_size += payload_size;
                self.buffer.push(payload);
            } else {
                break;
            }
        }

        std::mem::take(&mut self.buffer)
    }


}

pub enum BatchData {
    SD(SendData),
    CSP(ClientStatsPayload)
}

impl BatchData {
    fn len(&self) -> usize {
        match self {
            BatchData::SD(sd) => sd.len(),
            BatchData::CSP(csp) => csp.encoded_len()
        }
    }
    fn is_empty(&self) -> bool {
        match self {
            BatchData::SD(sd) => sd.is_empty(),
            BatchData::CSP(csp) => csp.encoded_len() == 0,
        }
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
        let mut aggregator = BatchAggregator::new(
            MAX_CONTENT_SIZE_BYTES_SD,
        );
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

        aggregator.add(BatchData::SD(payload.clone()));
        assert_eq!(aggregator.queue.len(), 1);
        assert_eq!(aggregator.queue[0].is_empty(), payload.is_empty());
    }

    #[test]
    fn test_get_batch() {
        let mut aggregator = BatchAggregator::new(
            MAX_CONTENT_SIZE_BYTES_SD,
        );
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

        aggregator.add(BatchData::SD(payload.clone()));
        assert_eq!(aggregator.queue.len(), 1);
        let batch = aggregator.get_batch();
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn test_get_batch_full_entries() {
        let mut aggregator = BatchAggregator::new(2);
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
        aggregator.add(BatchData::SD(payload.clone()));
        aggregator.add(BatchData::SD(payload.clone()));
        aggregator.add(BatchData::SD(payload.clone()));

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
