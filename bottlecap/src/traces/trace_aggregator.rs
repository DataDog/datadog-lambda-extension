use datadog_trace_utils::send_data::SendData;
use std::collections::VecDeque;

/// Maximum content size per payload uncompressed in bytes,
/// that the Datadog Trace API accepts. The value is 3.2 MB.
///
/// <https://github.com/DataDog/datadog-agent/blob/9d57c10a9eeb3916e661d35dbd23c6e36395a99d/pkg/trace/writer/trace.go#L27-L31>
pub const MAX_CONTENT_SIZE_BYTES: usize = (32 * 1_024 * 1_024) / 10;

#[allow(clippy::module_name_repetitions)]
pub struct TraceAggregator {
    queue: VecDeque<SendData>,
    max_content_size_bytes: usize,
    buffer: Vec<SendData>,
}

impl Default for TraceAggregator {
    fn default() -> Self {
        TraceAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes: MAX_CONTENT_SIZE_BYTES,
            buffer: Vec::with_capacity(MAX_CONTENT_SIZE_BYTES),
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
            buffer: Vec::with_capacity(max_content_size_bytes),
        }
    }

    pub fn add(&mut self, p: SendData) {
        self.queue.push_back(p);
    }

    pub fn get_batch(&mut self) -> Vec<SendData> {
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
