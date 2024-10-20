use std::collections::VecDeque;

use tracing::debug;

#[derive(Debug, Clone)]
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
            self.buffer.pop_front();
            self.buffer.push_back(invocation_context);
        } else {
            if self.get(&invocation_context.request_id).is_some() {
                self.remove(&invocation_context.request_id);
            }

            self.buffer.push_back(invocation_context);
        }
    }

    pub fn remove(&mut self, request_id: &String) -> Option<InvocationContext> {
        if let Some(i) = self
            .buffer
            .iter()
            .position(|context| context.request_id == *request_id)
        {
            return self.buffer.remove(i);
        }
        debug!("Context for request_id: {:?} not found", request_id);

        None
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
