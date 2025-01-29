use prost::Message;
use std::collections::VecDeque;

#[allow(clippy::module_name_repetitions)]
pub struct MessageAggregator<T>
where
    T: Message,
{
    queue: VecDeque<T>,
    max_content_size_bytes: usize,
    buffer: Vec<T>,
}

impl<T> MessageAggregator<T>
where
    T: Message,
{
    #[allow(dead_code)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(max_content_size_bytes: usize) -> Self {
        MessageAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes,
            buffer: Vec::with_capacity(max_content_size_bytes),
        }
    }

    pub fn add(&mut self, message: T) {
        self.queue.push_back(message);
    }

    pub fn get_batch(&mut self) -> Vec<T> {
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
