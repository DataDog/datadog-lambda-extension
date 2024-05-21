use std::collections::VecDeque;

use tracing::debug;

pub struct InvocationContext {
    pub request_id: String,
    pub runtime_duration_ms: f64,
}

#[allow(clippy::module_name_repetitions)]
pub struct InvocationContextBuffer {
    buffer: VecDeque<InvocationContext>,
}

impl Default for InvocationContextBuffer {
    fn default() -> Self {
        InvocationContextBuffer {
            buffer: VecDeque::<InvocationContext>::with_capacity(5),
        }
    }
}

impl InvocationContextBuffer {
    pub fn insert(&mut self, invocation_context: InvocationContext) {
        if self.buffer.len() == self.buffer.capacity() {
            debug!(
                "Buffer is full, dropping context for request_id: {:?}",
                invocation_context.request_id
            );
        } else {
            if self.get(&invocation_context.request_id).is_some() {
                self.remove(&invocation_context.request_id);
            }

            self.buffer.push_back(invocation_context);
        }
    }

    fn remove(&mut self, request_id: &String) {
        self.buffer
            .retain(|context| context.request_id != *request_id);
    }

    #[must_use]
    pub fn get(&self, request_id: &String) -> Option<&InvocationContext> {
        self.buffer
            .iter()
            .find(|context| context.request_id == *request_id)
    }

    pub fn add_runtime_duration(&mut self, request_id: &String, runtime_duration_ms: f64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.runtime_duration_ms = runtime_duration_ms;
        } else {
            self.insert(InvocationContext {
                request_id: request_id.to_string(),
                runtime_duration_ms,
            });
        }
    }
}
