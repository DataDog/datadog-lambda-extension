use datadog_trace_protobuf::pb::ClientStatsPayload;
use prost::Message;
use std::collections::VecDeque;

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
const MAX_CONTENT_SIZE_BYTES: usize = 3 * 1024 * 1024; // ~3MB

#[allow(clippy::module_name_repetitions)]
pub struct StatsAggregator {
    queue: VecDeque<ClientStatsPayload>,
    max_content_size_bytes: usize,
    buffer: Vec<ClientStatsPayload>,
}

impl Default for StatsAggregator {
    fn default() -> Self {
        StatsAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes: MAX_CONTENT_SIZE_BYTES,
            buffer: Vec::with_capacity(MAX_CONTENT_SIZE_BYTES),
        }
    }
}

impl StatsAggregator {
    #[allow(dead_code)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(max_content_size_bytes: usize) -> Self {
        StatsAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes,
            buffer: Vec::with_capacity(max_content_size_bytes),
        }
    }

    pub fn add(&mut self, message: ClientStatsPayload) {
        self.queue.push_back(message);
    }

    pub fn get_batch(&mut self) -> Vec<ClientStatsPayload> {
        let mut batch_size = 0;

        // Fill the batch
        while batch_size < self.max_content_size_bytes {
            if let Some(message) = self.queue.pop_front() {
                let message_size = message.encoded_len();

                // Put stats back in the queue
                if batch_size + message_size > self.max_content_size_bytes {
                    self.queue.push_front(message);
                    break;
                }
                batch_size += message_size;
                self.buffer.push(message);
            } else {
                break;
            }
        }

        std::mem::take(&mut self.buffer)
    }
}
