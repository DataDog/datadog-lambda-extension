use crate::{
    lifecycle::invocation::processor::MS_TO_NS, metrics::enhanced::lambda::EnhancedMetricData,
    traces::context::SpanContext,
};
use std::{
    collections::{HashMap, VecDeque},
    time::{SystemTime, UNIX_EPOCH},
};

use datadog_trace_protobuf::pb::Span;
use tracing::debug;

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    /// The timestamp when the context was created.
    created_at: i64,
    /// The unique identifier for the request.
    pub request_id: String,
    pub runtime_duration_ms: f64,
    pub enhanced_metric_data: Option<EnhancedMetricData>,
    /// The span representing the invocation of the function.
    ///
    /// Known as the `aws.lambda` span.
    pub invocation_span: Span,
    /// The span representing the cold start of the Lambda sandbox.
    ///
    /// This span is only present if the function is being invoked for the first time.
    pub cold_start_span: Option<Span>,
    /// The span used as placeholder for the invocation span by the tracer.
    ///
    /// In the tracer, this is created in order to have all children spans parented
    /// to a single span. This is useful when we reparent the tracer span children to
    /// the invocation span.
    ///
    /// This span is filtered out during chunk processing.
    pub tracer_span: Option<Span>,
    /// The extracted span context from the incoming request, if any.
    ///
    pub span_context: Option<SpanContext>,
}

#[allow(clippy::too_many_arguments)]
impl Context {
    #[must_use]
    pub fn new(
        created_at: i64,
        request_id: String,
        runtime_duration_ms: f64,
        enhanced_metric_data: Option<EnhancedMetricData>,
        invocation_span: Span,
        cold_start_span: Option<Span>,
        tracer_span: Option<Span>,
        span_context: Option<SpanContext>,
    ) -> Self {
        Context {
            created_at,
            request_id,
            runtime_duration_ms,
            enhanced_metric_data,
            invocation_span,
            cold_start_span,
            tracer_span,
            span_context,
        }
    }

    #[must_use]
    pub fn from_request_id(request_id: &str) -> Self {
        let mut context = Self::default();
        request_id.clone_into(&mut context.request_id);

        let now: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();
        context.created_at = now;

        context
    }
}

impl Default for Context {
    fn default() -> Self {
        Context {
            created_at: 0,
            request_id: String::new(),
            runtime_duration_ms: 0f64,
            enhanced_metric_data: None,
            invocation_span: Span::default(),
            cold_start_span: None,
            tracer_span: None,
            span_context: None,
        }
    }
}

#[allow(clippy::module_name_repetitions)]
pub struct ContextBuffer {
    /// The buffer of invocation contexts.
    ///
    /// The buffer is a queue of contexts that are used to store the context of the
    /// previous invocations.
    buffer: VecDeque<Context>,
    /// The buffer of unordered events.
    ///
    /// This buffer holds events that might not be ordered by the time they are processed.
    /// For correct processing, events are paired based on a common pattern.
    ///
    /// The expected order of events is:
    /// ```
    /// Invoke -> InvocationStart -> InvocationEnd -> PlatformRuntimeDone
    /// ```
    ///
    /// 1. The `Invoke` event is used to pair the `InvocationStart` event.
    /// If the `InvocationStart` event occurs before the `Invoke` event, it is stored in the buffer.
    /// When the `Invoke` event occurs, the `InvocationStart` event is popped from the buffer and paired.
    ///
    /// 2. The `InvocationEnd` event is used to pair the `PlatformRuntimeDone` event.
    /// Similarly, `PlatformRuntimeDone` occurs before `InvocationEnd` event, it is stored the buffer.
    /// Once `InvocationEnd` happens, the event is popped from the buffer and paired for processing.
    unordered_events_buffer: Vec<UnorderedEvents>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UnorderedEvents {
    Invoke(String),
    PlatformRuntimeDone(String),
    InvocationStart(HashMap<String, String>, Vec<u8>),
    InvocationEnd(HashMap<String, String>, Vec<u8>),
}

impl Default for ContextBuffer {
    /// Creates a new `ContextBuffer` with a default capacity of 5.
    ///
    fn default() -> Self {
        ContextBuffer {
            buffer: VecDeque::<Context>::with_capacity(5),
            unordered_events_buffer: Vec::with_capacity(20),
        }
    }
}

impl ContextBuffer {
    #[allow(dead_code)]
    fn with_capacity(capacity: usize) -> Self {
        ContextBuffer {
            buffer: VecDeque::<Context>::with_capacity(capacity),
            unordered_events_buffer: Vec::with_capacity(capacity),
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

    /// Returns a mutable reference to a `Context` from the buffer if found.
    ///
    #[must_use]
    pub fn get_mut(&mut self, request_id: &String) -> Option<&mut Context> {
        self.buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
    }

    #[must_use]
    pub fn get_closest_mut(&mut self, timestamp: i64) -> Option<&mut Context> {
        if self.buffer.is_empty() {
            return None;
        }

        let mut closest_context = None;
        let mut min_diff = i64::MAX;

        for context in &mut self.buffer {
            let diff = (context.created_at - timestamp).abs();
            if diff < min_diff {
                min_diff = diff;
                closest_context = Some(context);
            }
        }

        closest_context
    }

    /// Returns the `InvocationStart` event from the buffer if found.
    ///
    /// This is supposed to be called only inside the `on_invoke_event` method.
    ///
    /// If the `InvocationStart` event has occurred before, remove it from the queue
    /// and return the event, so the `on_invoke_event` method can process the
    /// `InvocationStart` event.
    ///
    /// If the `InvocationStart` event hasn't occurred yet, push the `Invoke` event to
    /// the queue so the `request_id` can be later used. Returns `None` in this case.
    pub fn get_invocation_start_event_data(
        &mut self,
        request_id: &str,
    ) -> Option<(HashMap<String, String>, Vec<u8>)> {
        let mut found_index = usize::MAX;

        for (i, event) in self.unordered_events_buffer.iter().enumerate() {
            if let UnorderedEvents::InvocationStart(_, _) = event {
                found_index = i;
                break;
            }
        }

        // `InvocationStart` event hasn't occurred yet, this is good,
        // push the Invoke event to the queue and return `None`
        if found_index == usize::MAX {
            self.unordered_events_buffer
                .push(UnorderedEvents::Invoke(request_id.to_owned()));
            return None;
        }

        // Bad scenario, we found an `InvocationStart`
        match self.unordered_events_buffer.remove(found_index) {
            UnorderedEvents::InvocationStart(headers, payload) => {
                Some((headers.clone(), payload.clone()))
            }
            _ => None,
        }
    }

    /// Returns the `Invoke` event from the buffer if found.
    ///
    /// This is supposed to be called only inside the `on_invocation_start` method.
    ///
    /// If the `Invoke` event has occurred before, remove it from the queue and
    /// return the event, so the `on_invocation_start` method can get the `request_id`
    /// to process the invocation start data.
    ///
    /// If the `Invoke` event hasn't occurred yet, push the `InvocationStart` event to
    /// the queue so the `headers` and `payload` can be later used. Returns `None` in this case.
    pub fn get_invoke_event_request_id(
        &mut self,
        headers: &HashMap<String, String>,
        payload: &[u8],
    ) -> Option<String> {
        let mut found_index = usize::MAX;
        for (i, event) in self.unordered_events_buffer.iter().enumerate() {
            if let UnorderedEvents::Invoke(_) = event {
                found_index = i;
                break;
            }
        }

        // `Invoke` event hasn't occurred yet, this is bad,
        // push the `InvocationStart` event to the queue and return `None`
        if found_index == usize::MAX {
            self.unordered_events_buffer.push(UnorderedEvents::InvocationStart(
                headers.clone(),
                payload.to_vec(),
            ));
            return None;
        }

        // Pop the Invoke event from the queue
        match self.unordered_events_buffer.remove(found_index) {
            UnorderedEvents::Invoke(request_id) => Some(request_id.clone()),
            _ => None,
        }
    }

    /// Returns the `PlatformRuntimeDone` event from the buffer if found.
    ///
    /// This is supposed to be called only inside the `on_invocation_end` method.
    ///
    /// If the `PlatformRuntimeDone` event has occurred before, remove it from the queue
    /// and return the event, so the `on_invocation_end` method can process itself with.
    ///
    /// If the `PlatformRuntimeDone` event hasn't occurred yet, push the `InvocationEnd` event
    /// to the queue so the `headers` and `payload` can be later used. Returns `None` in this case.
    pub fn get_platform_runtime_done_event_request_id(
        &mut self,
        headers: &HashMap<String, String>,
        payload: &[u8],
    ) -> Option<String> {
        let mut found_index = usize::MAX;
        for (i, event) in self.unordered_events_buffer.iter().enumerate() {
            if let UnorderedEvents::PlatformRuntimeDone(_) = event {
                found_index = i;
                break;
            }
        }

        // `PlatformRuntimeDone` hasn't occurred yet, this is good,
        // push the `InvocationEnd` event to the queue and return `None`
        if found_index == usize::MAX {
            self.unordered_events_buffer.push(UnorderedEvents::InvocationEnd(
                headers.clone(),
                payload.to_vec(),
            ));
            return None;
        }

        // Bad scenario, we found a `PlatformRuntimeDone`
        match self.unordered_events_buffer.remove(found_index) {
            UnorderedEvents::PlatformRuntimeDone(request_id) => Some(request_id),
            _ => None,
        }
    }

    /// Returns the `InvocationEnd` event from the buffer if found.
    ///
    /// This is supposed to be called only inside the `on_platform_runtime_done` method.
    ///
    /// If the `InvocationEnd` event has occurred before, remove it from the queue and
    /// return the event, so the `on_platform_runtime_done` can process the invocation end data.
    ///
    /// If the `InvocationEnd` event hasn't occurred yet, push the `PlatformRuntimeDone` event
    /// so the `request_id` can be later used. Returns `None` in this case.
    pub fn get_invocation_end_event_data(
        &mut self,
        request_id: &str,
    ) -> Option<(HashMap<String, String>, Vec<u8>)> {
        let mut found_index = usize::MAX;
        for (i, event) in self.unordered_events_buffer.iter().enumerate() {
            if let UnorderedEvents::InvocationEnd(_, _) = event {
                found_index = i;
                break;
            }
        }

        // `InvocationEnd` hasn't occurred yet, this is bad,
        // push the `PlatformRuntimeDone` event to the queue and return `None`
        if found_index == usize::MAX {
            self.unordered_events_buffer
                .push(UnorderedEvents::PlatformRuntimeDone(request_id.to_owned()));
            return None;
        }

        // Good scenario, we found an `InvocationEnd`
        match self.unordered_events_buffer.remove(found_index) {
            UnorderedEvents::InvocationEnd(headers, payload) => {
                Some((headers.clone(), payload.clone()))
            }
            _ => None,
        }
    }

    /// Creates a new `Context` and adds it to the buffer given the `request_id`
    /// and the `invocation_span`.
    ///
    pub fn create_context(&mut self, request_id: &str, invocation_span: Span) {
        let mut context = Context::from_request_id(request_id);
        context.invocation_span = invocation_span;
        context
            .invocation_span
            .meta
            .insert("request_id".to_string(), request_id.to_string());

        self.insert(context);
    }

    /// Adds the start time to the invocation span of a `Context` in the buffer.
    ///
    pub fn add_start_time(&mut self, request_id: &String, start_time: i64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.invocation_span.start = start_time;
        } else {
            debug!("Could not add start time - context not found");
        }
    }

    /// Adds the runtime duration to a `Context` in the buffer.
    ///
    #[allow(clippy::cast_possible_truncation)]
    pub fn add_runtime_duration(&mut self, request_id: &String, runtime_duration_ms: f64) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context.runtime_duration_ms = runtime_duration_ms;
            // `round` is intentionally meant to be a whole integer
            context.invocation_span.duration = (runtime_duration_ms * MS_TO_NS).round() as i64;
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

    /// Adds the tracer span to a `Context` in the buffer.
    ///
    pub fn add_tracer_span(&mut self, request_id: &String, tracer_span: &Span) {
        if let Some(context) = self
            .buffer
            .iter_mut()
            .find(|context| context.request_id == *request_id)
        {
            context
                .invocation_span
                .meta
                .extend(tracer_span.meta.clone());
            context
                .invocation_span
                .meta_struct
                .extend(tracer_span.meta_struct.clone());
            context
                .invocation_span
                .metrics
                .extend(tracer_span.metrics.clone());

            context.tracer_span = Some(tracer_span.clone());
        } else {
            debug!("Could not add tracer span - context not found");
        }
    }

    /// Returns the size of the buffer.
    ///
    #[must_use]
    pub fn size(&self) -> usize {
        self.buffer.len()
    }

    /// Returns if the buffer is empty.
    ///
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::proc::{CPUData, NetworkData};
    use std::collections::HashMap;
    use tokio::sync::watch;

    use super::*;

    #[test]
    fn test_insert() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::from_request_id(&request_id);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let request_id_2 = String::from("2");
        let context = Context::from_request_id(&request_id_2);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 2);
        assert_eq!(buffer.get(&request_id_2).unwrap(), &context);

        // This should replace the first context
        let request_id_3 = String::from("3");
        let context = Context::from_request_id(&request_id_3);
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
        let context = Context::from_request_id(&request_id);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let request_id_2 = String::from("2");
        let context = Context::from_request_id(&request_id_2);
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
        let context = Context::from_request_id(&request_id);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let request_id_2 = String::from("2");
        let context = Context::from_request_id(&request_id_2);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 2);
        assert_eq!(buffer.get(&request_id_2).unwrap(), &context);

        // Get a context that doesn't exist
        let unexistent_request_id = String::from("unexistent");
        assert!(buffer.get(&unexistent_request_id).is_none());
    }

    #[test]
    fn test_add_start_time() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::from_request_id(&request_id);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        buffer.add_start_time(&request_id, 100);
        assert_eq!(buffer.get(&request_id).unwrap().invocation_span.start, 100);
    }

    #[test]
    fn test_add_runtime_duration() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::from_request_id(&request_id);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        buffer.add_runtime_duration(&request_id, 100f64);
        assert!(
            (buffer.get(&request_id).unwrap().runtime_duration_ms - 100f64).abs() < f64::EPSILON
        );
    }

    #[test]
    fn test_add_enhanced_metric_data() {
        let mut buffer = ContextBuffer::with_capacity(2);

        let request_id = String::from("1");
        let context = Context::from_request_id(&request_id);
        buffer.insert(context.clone());
        assert_eq!(buffer.size(), 1);
        assert_eq!(buffer.get(&request_id).unwrap(), &context);

        let network_offset = Some(NetworkData {
            rx_bytes: 180f64,
            tx_bytes: 254.0,
        });

        let mut individual_cpu_idle_times = HashMap::new();
        individual_cpu_idle_times.insert("cpu0".to_string(), 10f64);
        individual_cpu_idle_times.insert("cpu1".to_string(), 20f64);
        let cpu_offset = Some(CPUData {
            total_user_time_ms: 100f64,
            total_system_time_ms: 53.0,
            total_idle_time_ms: 20f64,
            individual_cpu_idle_times,
        });

        let uptime_offset = Some(50f64);
        let (tmp_chan_tx, _) = watch::channel(());
        let (process_chan_tx, _) = watch::channel(());

        let enhanced_metric_data = Some(EnhancedMetricData {
            network_offset,
            cpu_offset,
            uptime_offset,
            tmp_chan_tx,
            process_chan_tx,
        });

        buffer.add_enhanced_metric_data(&request_id, enhanced_metric_data.clone());
        assert_eq!(
            buffer.get(&request_id).unwrap().enhanced_metric_data,
            enhanced_metric_data,
        );
    }
}
