use crate::metrics::enhanced::lambda::EnhancedMetricData;
use std::collections::VecDeque;

use tracing::debug;

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub request_id: String,
    pub runtime_duration_ms: f64,
    pub init_duration_ms: f64,
    pub start_time: i64,
    pub enhanced_metric_data: Option<EnhancedMetricData>,
}

impl Context {
    #[must_use]
    pub fn new(
        request_id: String,
        runtime_duration_ms: f64,
        init_duration_ms: f64,
        start_time: i64,
        enhanced_metric_data: Option<EnhancedMetricData>,
    ) -> Self {
        Context {
            request_id,
            runtime_duration_ms,
            init_duration_ms,
            start_time,
            enhanced_metric_data,
        }
    }
}

#[allow(clippy::module_name_repetitions)]
pub struct ContextBuffer {
    buffer: VecDeque<Context>,
}

impl Default for ContextBuffer {
    /// Creates a new `ContextBuffer` with a default capacity of 5.
    ///
    fn default() -> Self {
        ContextBuffer {
            buffer: VecDeque::<Context>::with_capacity(5),
        }
    }
}

impl ContextBuffer {
    #[allow(dead_code)]
    fn with_capacity(capacity: usize) -> Self {
        ContextBuffer {
            buffer: VecDeque::<Context>::with_capacity(capacity),
        }
    }

    /// Inserts a context into the buffer. If the buffer is full, the oldest `Context` is removed.
    ///
    fn insert(&mut self, context: Context) {
        if self.size() == self.buffer.capacity() {
            self.buffer.pop_front();
            self.buffer.push_back(context);
        } else {
            if self.get(&context.request_id).is_some() {
                self.remove(&context.request_id);
            }

            self.buffer.push_back(context);
        }
    }

    /// Removes a context from the buffer. Returns the removed `Context` if found.
    ///
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

    /// Returns a reference to a `Context` from the buffer if found.
    ///
    #[must_use]
    pub fn get(&self, request_id: &String) -> Option<&Context> {
        self.buffer
            .iter()
            .find(|context| context.request_id == *request_id)
    }

    /// Creates a new `Context` and adds it to the buffer.
    ///
    pub fn create_context(&mut self, request_id: String) {
        self.insert(Context::new(request_id, 0.0, 0.0, 0, None));
    }

    /// Adds the init duration to a `Context` in the buffer.
    ///
    pub fn add_init_duration(&mut self, request_id: &String, init_duration_ms: f64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.init_duration_ms = init_duration_ms;
        } else {
            debug!("Could not add init duration - context not found");
        }
    }

    /// Adds the start time to a `Context` in the buffer.
    ///
    pub fn add_start_time(&mut self, request_id: &String, start_time: i64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.start_time = start_time;
        } else {
            debug!("Could not add start time - context not found");
        }
    }

    /// Adds the runtime duration to a `Context` in the buffer.
    ///
    pub fn add_runtime_duration(&mut self, request_id: &String, runtime_duration_ms: f64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.runtime_duration_ms = runtime_duration_ms;
        } else {
            debug!("Could not add runtime duration - context not found");
        }
    }

    /// Adds the network offset to a `Context` in the buffer.
    ///
    pub fn add_enhanced_metric_data(
        &mut self,
        request_id: &String,
        enhanced_metric_data: Option<EnhancedMetricData>,
    ) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.enhanced_metric_data = enhanced_metric_data;
        } else {
            debug!("Could not add network offset - context not found");
        }
    }

    /// Returns the size of the buffer.
    ///
    #[must_use]
    pub fn size(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::proc::{CPUData, NetworkData};
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_insert() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::new(request_id.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let request_id_2 = String::from("2");
        let context = Context::new(request_id_2.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 2);
        assert_eq!(buffer.get(&request_id_2).unwrap(), &context);

        // This should replace the first context
        let request_id_3 = String::from("3");
        let context = Context::new(request_id_3.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 2);
        assert_eq!(buffer.get(&request_id_3).unwrap(), &context);

        // First context should be None
        assert!(buffer.get(&request_id).is_none());
    }

    #[test]
    fn test_remove() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::new(request_id.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let request_id_2 = String::from("2");
        let context = Context::new(request_id_2.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 2);
        assert_eq!(buffer.get(&request_id_2).unwrap(), &context);

        // Remove the first context
        assert_eq!(buffer.remove(&request_id).unwrap().request_id, request_id);
        // Size is reduced by 1
        assert_eq!(buffer.size(), 1);
        assert!(buffer.get(&request_id).is_none());

        // Remove a context that doesn't exist
        let unexistent_request_id = String::from("unexistent");
        assert!(buffer.remove(&unexistent_request_id).is_none());
    }

    #[test]
    fn test_get() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::new(request_id.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let request_id_2 = String::from("2");
        let context = Context::new(request_id_2.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 2);
        assert_eq!(buffer.get(&request_id_2).unwrap(), &context);

        // Get a context that doesn't exist
        let unexistent_request_id = String::from("unexistent");
        assert!(buffer.get(&unexistent_request_id).is_none());
    }

    #[test]
    fn test_add_init_duration() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::new(request_id.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        buffer.add_init_duration(&request_id, 100.0);
        assert_eq!(buffer.get(&request_id).unwrap().init_duration_ms, 100.0);
    }

    #[test]
    fn test_add_start_time() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::new(request_id.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        buffer.add_start_time(&request_id, 100);
        assert_eq!(buffer.get(&request_id).unwrap().start_time, 100);
    }

    #[test]
    fn test_add_runtime_duration() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::new(request_id.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        buffer.add_runtime_duration(&request_id, 100.0);
        assert_eq!(buffer.get(&request_id).unwrap().runtime_duration_ms, 100.0);
    }

    #[test]
    fn test_add_enhanced_metric_data() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::new(request_id.clone(), 0.0, 0.0, 0, None);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let network_offset = Some(NetworkData {
            rx_bytes: 180.0,
            tx_bytes: 254.0,
        });

        let mut individual_cpu_idle_times = HashMap::new();
        individual_cpu_idle_times.insert("cpu0".to_string(), 10.0);
        individual_cpu_idle_times.insert("cpu1".to_string(), 20.0);
        let cpu_offset = Some(CPUData {
            total_user_time_ms: 100.0,
            total_system_time_ms: 53.0,
            total_idle_time_ms: 20.0,
            individual_cpu_idle_times: individual_cpu_idle_times,
        });

        let uptime_offset = Some(50.0);

        let enhanced_metric_data = Some(EnhancedMetricData {
            network_offset,
            cpu_offset,
            uptime_offset,
        });

        buffer.add_enhanced_metric_data(&request_id, enhanced_metric_data.clone());
        assert_eq!(
            buffer.get(&request_id).unwrap().enhanced_metric_data,
            enhanced_metric_data,
        );
    }
}
