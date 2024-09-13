use std::collections::VecDeque;

use tracing::debug;

#[derive(Debug, Clone)]
pub struct Context {
    pub request_id: String,
    pub runtime_duration_ms: f64,
    pub init_duration_ms: f64,
    pub start_time: i64,
}

impl Context {
    #[must_use]
    pub fn new(
        request_id: String,
        runtime_duration_ms: f64,
        init_duration_ms: f64,
        start_time: i64,
    ) -> Self {
        Context {
            request_id,
            runtime_duration_ms,
            init_duration_ms,
            start_time,
        }
    }
}

#[allow(clippy::module_name_repetitions)]
pub struct ContextBuffer {
    buffer: VecDeque<Context>,
}

impl Default for ContextBuffer {
    fn default() -> Self {
        ContextBuffer {
            buffer: VecDeque::<Context>::with_capacity(5),
        }
    }
}

impl ContextBuffer {
    fn insert(&mut self, context: Context) {
        if self.buffer.len() == self.buffer.capacity() {
            self.buffer.pop_front();
            self.buffer.push_back(context);
        } else {
            if self.get(&context.request_id).is_some() {
                self.remove(&context.request_id);
            }

            self.buffer.push_back(context);
        }
    }

    pub fn remove(&mut self, request_id: &String) -> Option<Context> {
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
    pub fn get(&self, request_id: &String) -> Option<&Context> {
        self.buffer
            .iter()
            .find(|context| context.request_id == *request_id)
    }

    pub fn add_init_duration(&mut self, request_id: &String, init_duration_ms: f64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.init_duration_ms = init_duration_ms;
        } else {
            self.insert(Context::new(request_id.clone(), 0.0, init_duration_ms, 0));
        }
    }

    pub fn add_start_time(&mut self, request_id: &String, start_time: i64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.start_time = start_time;
        } else {
            self.insert(Context::new(request_id.clone(), 0.0, 0.0, start_time));
        }
    }

    pub fn add_runtime_duration(&mut self, request_id: &String, runtime_duration_ms: f64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.runtime_duration_ms = runtime_duration_ms;
        } else {
            self.insert(Context::new(
                request_id.clone(),
                runtime_duration_ms,
                0.0,
                0,
            ));
        }
    }
}
