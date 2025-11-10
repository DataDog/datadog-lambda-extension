use crate::{
    lifecycle::invocation::processor::MS_TO_NS, metrics::enhanced::lambda::EnhancedMetricData,
    traces::context::SpanContext,
};
use std::{
    collections::{HashMap, VecDeque},
    time::{SystemTime, UNIX_EPOCH},
};

use libdd_trace_protobuf::pb::Span;
use serde_json::Value;
use tracing::debug;

#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    /// The timestamp when the context was created.
    created_at: i64,
    pub request_id: String,
    pub runtime_duration_ms: f64,
    pub enhanced_metric_data: Option<EnhancedMetricData>,
    /// The span representing the invocation of the function.
    ///
    /// Known as the `aws.lambda` span.
    pub invocation_span: Span,
    pub runtime_done_received: bool,
    /// The span used as placeholder for the invocation span by the tracer.
    ///
    /// In the tracer, this is created in order to have all children spans parented
    /// to a single span. This is useful when we reparent the tracer span children to
    /// the invocation span.
    ///
    /// This span is filtered out during chunk processing.
    pub tracer_span: Option<Span>,
    /// The span representing the cold start of the Lambda sandbox.
    ///
    /// This span is only present if the function is being invoked for the first time.
    pub cold_start_span: Option<Span>,
    /// The span representing the `SnapStart` restore phase.
    ///
    /// This span is only present if the function is using `SnapStart` and being invoked for the first time.
    pub snapstart_restore_span: Option<Span>,
    /// The extracted span context from the incoming request, used for distributed
    /// tracing.
    ///
    pub extracted_span_context: Option<SpanContext>,
}

/// Struct containing the information needed to reparent a span.
/// The struct contains initially the span ID of an invocation span, the lambda request ID
/// causing the invocation, and the parent found (0 if no inferred spans or existing parent were
/// found).
///
/// When receiving spans, the trace id for this request will be guessed based on the order of
/// incoming spans. So it holds true when at least one span related to invocation N is received
/// by the extension before the spans of request N+1
#[derive(Clone, Debug)]
pub struct ReparentingInfo {
    pub request_id: String,
    pub invocation_span_id: u64,
    pub parent_id_to_reparent: u64,

    pub guessed_trace_id: u64,
    pub needs_trace_id: bool,
}

#[allow(clippy::too_many_arguments)]
impl Context {
    #[must_use]
    pub fn from_request_id(request_id: &str) -> Self {
        let mut context = Self::default();
        request_id.clone_into(&mut context.request_id);

        context
    }
}

impl Default for Context {
    fn default() -> Self {
        let now: i64 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
            .try_into()
            .unwrap_or_default();

        Context {
            created_at: now,
            request_id: String::new(),
            runtime_duration_ms: 0f64,
            enhanced_metric_data: None,
            invocation_span: Span::default(),
            runtime_done_received: false,
            cold_start_span: None,
            snapstart_restore_span: None,
            tracer_span: None,
            extracted_span_context: None,
        }
    }
}

#[allow(clippy::module_name_repetitions)]
pub struct ContextBuffer {
    /// The buffer of invocation contexts.
    ///
    /// The buffer is a queue of the last 5 invocation contexts, including
    /// the current one.
    buffer: VecDeque<Context>,
    /// The buffers of unordered events.
    ///
    /// These buffers hold events that might not be ordered by the time they are processed.
    /// For correct processing, events are paired based on a common pattern.
    ///
    /// The expected order of events is:
    ///
    /// Invoke -> `UniversalInstrumentationStart` -> `UniversalInstrumentationEnd` -> `PlatformRuntimeDone`
    ///
    ///
    /// 1. The `Invoke` event is used to pair the `UniversalInstrumentationStart` event.
    ///
    ///    If the `UniversalInstrumentationStart` event occurs before the `Invoke` event, it is stored in the buffer.
    ///    When the `Invoke` event occurs, the `UniversalInstrumentationStart` event is popped from the buffer and paired.
    ///
    /// 2. The `UniversalInstrumentationEnd` event is used to pair the `PlatformRuntimeDone` event.
    ///
    ///    Similarly, `PlatformRuntimeDone` occurs before `UniversalInstrumentationEnd` event, it is stored the buffer.
    ///    Once `UniversalInstrumentationEnd` happens, the event is popped from the buffer and paired for processing.
    invoke_events_request_ids: VecDeque<String>,
    platform_runtime_done_events_request_ids: VecDeque<String>,
    universal_instrumentation_start_events: VecDeque<UniversalInstrumentationData>,
    universal_instrumentation_end_events: VecDeque<UniversalInstrumentationData>,
    /// Managed instance mode: Request ID-aware buffers for concurrent invocations
    /// Using hash map for O(1) lookups by request id
    universal_instrumentation_start_events_with_id:
        HashMap<String, UniversalInstrumentationDataWithRequestId>,
    universal_instrumentation_end_events_with_id:
        HashMap<String, UniversalInstrumentationDataWithRequestId>,
    pub sorted_reparenting_info: VecDeque<ReparentingInfo>,
}

struct UniversalInstrumentationData {
    headers: HashMap<String, String>,
    payload_value: Value,
}

/// Enhanced version with `request_id` for managed instance mode pairing
pub struct UniversalInstrumentationDataWithRequestId {
    pub request_id: String,
    pub headers: HashMap<String, String>,
    pub payload_value: Value,
}

impl Default for ContextBuffer {
    /// Creates a new `ContextBuffer` with a default capacity of 500
    /// This gives us enough capacity to process events which may be delayed due to async tasks
    /// piling up preventing us from reading them quickly enough
    ///
    fn default() -> Self {
        ContextBuffer::with_capacity(500)
    }
}

impl ContextBuffer {
    #[allow(dead_code)]
    fn with_capacity(capacity: usize) -> Self {
        ContextBuffer {
            buffer: VecDeque::<Context>::with_capacity(capacity),
            invoke_events_request_ids: VecDeque::with_capacity(capacity),
            platform_runtime_done_events_request_ids: VecDeque::with_capacity(capacity),
            universal_instrumentation_start_events: VecDeque::with_capacity(capacity),
            universal_instrumentation_end_events: VecDeque::with_capacity(capacity),
            universal_instrumentation_start_events_with_id: HashMap::with_capacity(capacity),
            universal_instrumentation_end_events_with_id: HashMap::with_capacity(capacity),
            sorted_reparenting_info: VecDeque::with_capacity(capacity),
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

    /// Returns the `UniversalInstrumentationStart` event from the buffer if found.
    ///
    /// None if the `Invoke` event hasn't occurred yet.
    pub fn pair_invoke_event(
        &mut self,
        request_id: &str,
    ) -> Option<(HashMap<String, String>, Value)> {
        if let Some(UniversalInstrumentationData {
            headers,
            payload_value,
        }) = self.universal_instrumentation_start_events.pop_front()
        {
            // Bad scenario, we found an `UniversalInstrumentationStart`
            Some((headers, payload_value))
        } else {
            // `UniversalInstrumentationStart` event hasn't occurred yet, this is good,
            // push the Invoke event to the queue and return `None`
            self.invoke_events_request_ids
                .push_back(request_id.to_owned());
            None
        }
    }

    /// Returns the `Invoke` event from the buffer if found.
    ///
    /// None if the `UniversalInstrumentationStart` event hasn't occurred yet.
    pub fn pair_universal_instrumentation_start_event(
        &mut self,
        headers: &HashMap<String, String>,
        payload_value: &Value,
    ) -> Option<String> {
        if let Some(request_id) = self.invoke_events_request_ids.pop_front() {
            // Bad scenario, we found an `UniversalInstrumentationStart`
            Some(request_id)
        } else {
            // `Invoke` event hasn't occurred yet, this is bad,
            // push the `UniversalInstrumentationStart` event to the queue and return `None`
            self.universal_instrumentation_start_events
                .push_back(UniversalInstrumentationData {
                    headers: headers.clone(),
                    payload_value: payload_value.clone(),
                });
            None
        }
    }

    /// Returns the `PlatformRuntimeDone` event from the buffer if found.
    ///
    /// None if the `UniversalInstrumentationEnd` event hasn't occurred yet.
    pub fn pair_universal_instrumentation_end_event(
        &mut self,
        headers: &HashMap<String, String>,
        payload_value: &Value,
    ) -> Option<String> {
        if let Some(request_id) = self.platform_runtime_done_events_request_ids.pop_front() {
            // Bad scenario, we found a `PlatformRuntimeDone`
            Some(request_id)
        } else {
            // `PlatformRuntimeDone` hasn't occurred yet, this is good,
            // push the `UniversalInstrumentationEnd` event to the queue and return `None`
            self.universal_instrumentation_end_events
                .push_back(UniversalInstrumentationData {
                    headers: headers.clone(),
                    payload_value: payload_value.clone(),
                });
            None
        }
    }

    /// Returns the `UniversalInstrumentationEnd` event from the buffer if found.
    ///
    /// None if the `PlatformRuntimeDone` event hasn't occurred yet.
    pub fn pair_platform_runtime_done_event(
        &mut self,
        request_id: &str,
    ) -> Option<(HashMap<String, String>, Value)> {
        if let Some(UniversalInstrumentationData {
            headers,
            payload_value,
        }) = self.universal_instrumentation_end_events.pop_front()
        {
            // Good scenario, we found an `UniversalInstrumentationEnd`
            Some((headers, payload_value))
        } else {
            // `UniversalInstrumentationEnd` hasn't occurred yet, this is bad,
            // push the `PlatformRuntimeDone` event to the queue and return `None`
            self.platform_runtime_done_events_request_ids
                .push_back(request_id.to_owned());
            None
        }
    }

    // ========================================================================
    // Managed Instance Mode: Request ID-Based Pairing Methods
    // ========================================================================

    /// Managed instance mode: Pair `UniversalInstrumentationStart` with `request_id`.
    /// Returns true if the Invoke event already occurred (immediate pairing).
    /// Returns false if the event was buffered (Invoke hasn't happened yet).
    pub fn pair_universal_instrumentation_start_with_request_id(
        &mut self,
        request_id: &str,
        headers: &HashMap<String, String>,
        payload_value: &Value,
    ) -> bool {
        // Check if this request_id is waiting in the invoke_events queue
        if let Some(pos) = self
            .invoke_events_request_ids
            .iter()
            .position(|id| id == request_id)
        {
            // Found matching Invoke event, remove it and process immediately
            self.invoke_events_request_ids.remove(pos);
            return true;
        }

        // Invoke hasn't happened yet, buffer this event with request_id using HashMap for O(1) lookup
        self.universal_instrumentation_start_events_with_id.insert(
            request_id.to_string(),
            UniversalInstrumentationDataWithRequestId {
                request_id: request_id.to_string(),
                headers: headers.clone(),
                payload_value: payload_value.clone(),
            },
        );
        false
    }

    /// Managed instance mode: When Invoke event arrives, retrieve and remove buffered
    /// `UniversalInstrumentationStart` with matching `request_id`.
    pub fn take_universal_instrumentation_start_for_request_id(
        &mut self,
        request_id: &str,
    ) -> Option<UniversalInstrumentationDataWithRequestId> {
        self.universal_instrumentation_start_events_with_id
            .remove(request_id)
    }

    /// Managed instance mode: Pair `UniversalInstrumentationEnd` with `request_id`.
    /// Returns true if `PlatformRuntimeDone` already occurred (immediate pairing).
    /// Returns false if the event was buffered (`RuntimeDone` hasn't happened yet).
    pub fn pair_universal_instrumentation_end_with_request_id(
        &mut self,
        request_id: &str,
        headers: &HashMap<String, String>,
        payload_value: &Value,
    ) -> bool {
        // Check if this request_id is waiting in the platform_runtime_done queue
        if let Some(pos) = self
            .platform_runtime_done_events_request_ids
            .iter()
            .position(|id| id == request_id)
        {
            // Found matching RuntimeDone event, remove it and process immediately
            self.platform_runtime_done_events_request_ids.remove(pos);
            return true;
        }

        // RuntimeDone hasn't happened yet, buffer this event with request_id using HashMap for O(1) lookup
        self.universal_instrumentation_end_events_with_id.insert(
            request_id.to_string(),
            UniversalInstrumentationDataWithRequestId {
                request_id: request_id.to_string(),
                headers: headers.clone(),
                payload_value: payload_value.clone(),
            },
        );
        false
    }

    /// Managed instance mode: When `PlatformRuntimeDone` event arrives, retrieve and remove buffered
    /// `UniversalInstrumentationEnd` with matching `request_id`.
    pub fn take_universal_instrumentation_end_for_request_id(
        &mut self,
        request_id: &str,
    ) -> Option<UniversalInstrumentationDataWithRequestId> {
        self.universal_instrumentation_end_events_with_id
            .remove(request_id)
    }

    /// Creates a new `Context` and adds it to the buffer given the `request_id`
    /// and the `invocation_span`.
    ///
    pub fn start_context(&mut self, request_id: &str, invocation_span: Span) {
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

    #[must_use]
    pub fn get_context_with_cold_start(&mut self) -> Option<&mut Context> {
        self.buffer
            .iter_mut()
            .find(|context| context.cold_start_span.is_some())
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
    use serde_json::json;
    use std::collections::HashMap;

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

    #[test]
    fn test_get_context_with_cold_start() {
        let mut buffer = ContextBuffer::with_capacity(2);

        // Create a context with no cold start span
        let request_id = String::from("1");
        let context = Context::from_request_id(&request_id);
        buffer.insert(context.clone());

        // Should return None when no cold start span exists
        assert!(buffer.get_context_with_cold_start().is_none());

        // Create a context with a cold start span
        let request_id_2 = String::from("2");
        let mut context_2 = Context::from_request_id(&request_id_2);
        let cold_start_span = Span {
            name: "aws.lambda.cold_start".to_string(),
            span_id: 12345,
            ..Span::default()
        };
        context_2.cold_start_span = Some(cold_start_span);
        buffer.insert(context_2);

        // Should return the cold start span
        let context = buffer.get_context_with_cold_start();
        assert!(context.is_some());
        let cold_start_span = &context.as_ref().unwrap().cold_start_span;
        assert!(cold_start_span.is_some());
        assert_eq!(
            cold_start_span.as_ref().unwrap().name,
            "aws.lambda.cold_start"
        );
    }

    #[test]
    fn test_pair_invoke_event() {
        let mut buffer = ContextBuffer::with_capacity(2);
        let request_id = "test-request-1".to_string();
        let mut headers = HashMap::new();
        headers.insert("test-header".to_string(), "test-value".to_string());
        let payload = json!({ "test": "payload" });

        // Test case 1: UniversalInstrumentationStart arrives before Invoke
        buffer
            .universal_instrumentation_start_events
            .push_back(UniversalInstrumentationData {
                headers: headers.clone(),
                payload_value: payload.clone(),
            });

        // When Invoke arrives, it should pair with the existing UniversalInstrumentationStart
        let result = buffer.pair_invoke_event(&request_id);
        assert!(result.is_some());
        let (paired_headers, paired_payload) = result.unwrap();
        assert_eq!(paired_headers, headers);
        assert_eq!(paired_payload, payload);

        // Test case 2: Invoke arrives before UniversalInstrumentationStart
        let request_id2 = "test-request-2".to_string();
        let result = buffer.pair_invoke_event(&request_id2);
        assert!(result.is_none());
        assert_eq!(buffer.invoke_events_request_ids.len(), 1);
        assert_eq!(
            buffer.invoke_events_request_ids.front().unwrap(),
            &request_id2
        );
    }

    #[test]
    fn test_pair_universal_instrumentation_start_event() {
        let mut buffer = ContextBuffer::with_capacity(2);
        let request_id = "test-request-1".to_string();
        let mut headers = HashMap::new();
        headers.insert("test-header".to_string(), "test-value".to_string());
        let payload = json!({ "test": "payload" });

        // Test case 1: Invoke arrives before UniversalInstrumentationStart
        buffer
            .invoke_events_request_ids
            .push_back(request_id.clone());

        // When UniversalInstrumentationStart arrives, it should pair with the existing Invoke
        let result = buffer.pair_universal_instrumentation_start_event(&headers, &payload);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), request_id);

        // Test case 2: UniversalInstrumentationStart arrives before Invoke
        let result = buffer.pair_universal_instrumentation_start_event(&headers, &payload);
        assert!(result.is_none());
        assert_eq!(buffer.universal_instrumentation_start_events.len(), 1);
        let stored_event = buffer
            .universal_instrumentation_start_events
            .front()
            .unwrap();
        assert_eq!(stored_event.headers, headers);
        assert_eq!(stored_event.payload_value, payload);
    }

    #[test]
    fn test_pair_universal_instrumentation_end_event() {
        let mut buffer = ContextBuffer::with_capacity(2);
        let request_id = "test-request-1".to_string();
        let mut headers = HashMap::new();
        headers.insert("test-header".to_string(), "test-value".to_string());
        let payload = json!({ "test": "payload" });

        // Test case 1: PlatformRuntimeDone arrives before UniversalInstrumentationEnd
        buffer
            .platform_runtime_done_events_request_ids
            .push_back(request_id.clone());

        // When UniversalInstrumentationEnd arrives, it should pair with the existing PlatformRuntimeDone
        let result = buffer.pair_universal_instrumentation_end_event(&headers, &payload);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), request_id);

        // Test case 2: UniversalInstrumentationEnd arrives before PlatformRuntimeDone
        let result = buffer.pair_universal_instrumentation_end_event(&headers, &payload);
        assert!(result.is_none());
        assert_eq!(buffer.universal_instrumentation_end_events.len(), 1);
        let stored_event = buffer.universal_instrumentation_end_events.front().unwrap();
        assert_eq!(stored_event.headers, headers);
        assert_eq!(stored_event.payload_value, payload);
    }

    #[test]
    fn test_pair_platform_runtime_done_event() {
        let mut buffer = ContextBuffer::with_capacity(2);
        let request_id = "test-request-1".to_string();
        let mut headers = HashMap::new();
        headers.insert("test-header".to_string(), "test-value".to_string());
        let payload = json!({ "test": "payload" });

        // Test case 1: UniversalInstrumentationEnd arrives before PlatformRuntimeDone
        buffer
            .universal_instrumentation_end_events
            .push_back(UniversalInstrumentationData {
                headers: headers.clone(),
                payload_value: payload.clone(),
            });

        // When PlatformRuntimeDone arrives, it should pair with the existing UniversalInstrumentationEnd
        let result = buffer.pair_platform_runtime_done_event(&request_id);
        assert!(result.is_some());
        let (paired_headers, paired_payload) = result.unwrap();
        assert_eq!(paired_headers, headers);
        assert_eq!(paired_payload, payload);

        // Test case 2: PlatformRuntimeDone arrives before UniversalInstrumentationEnd
        let request_id2 = "test-request-2".to_string();
        let result = buffer.pair_platform_runtime_done_event(&request_id2);
        assert!(result.is_none());
        assert_eq!(buffer.platform_runtime_done_events_request_ids.len(), 1);
        assert_eq!(
            buffer
                .platform_runtime_done_events_request_ids
                .front()
                .unwrap(),
            &request_id2
        );
    }

    #[test]
    fn test_event_ordering() {
        let mut buffer = ContextBuffer::with_capacity(2);
        let request_id = "test-request-1".to_string();
        let mut headers = HashMap::new();
        headers.insert("test-header".to_string(), "test-value".to_string());
        let payload = json!({ "test": "payload" });

        // Test the complete flow with events arriving in different orders
        // Case 1: Events arrive in reverse order
        buffer
            .universal_instrumentation_end_events
            .push_back(UniversalInstrumentationData {
                headers: headers.clone(),
                payload_value: payload.clone(),
            });
        buffer
            .platform_runtime_done_events_request_ids
            .push_back(request_id.clone());

        // When UniversalInstrumentationEnd arrives, it should pair with PlatformRuntimeDone
        let result = buffer.pair_universal_instrumentation_end_event(&headers, &payload);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), request_id);

        // Case 2: Events arrive in correct order
        let request_id2 = "test-request-2".to_string();
        buffer
            .invoke_events_request_ids
            .push_back(request_id2.clone());
        let result = buffer.pair_universal_instrumentation_start_event(&headers, &payload);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), request_id2);
    }

    // ========================================================================
    // Managed Instance Mode Tests: Request ID-Based Pairing
    // ========================================================================

    #[test]
    fn test_managed_instance_universal_instrumentation_start_invoke_first() {
        // Test case: Invoke arrives before UniversalInstrumentationStart (common case)
        let mut buffer = ContextBuffer::with_capacity(10);
        let request_id = "req-123";
        let mut headers = HashMap::new();
        headers.insert("test-header".to_string(), "test-value".to_string());
        let payload = json!({ "test": "payload" });

        // Simulate Invoke event arriving first
        buffer
            .invoke_events_request_ids
            .push_back(request_id.to_string());

        // UniversalInstrumentationStart arrives - should pair immediately
        let result = buffer
            .pair_universal_instrumentation_start_with_request_id(request_id, &headers, &payload);
        assert!(result, "Should return true when Invoke already happened");
        assert_eq!(
            buffer.invoke_events_request_ids.len(),
            0,
            "Should remove paired Invoke event"
        );
        assert_eq!(
            buffer.universal_instrumentation_start_events_with_id.len(),
            0,
            "Should not buffer when immediate pairing occurs"
        );
    }

    #[test]
    fn test_managed_instance_universal_instrumentation_start_buffering() {
        // Test case: UniversalInstrumentationStart arrives before Invoke (out of order)
        let mut buffer = ContextBuffer::with_capacity(10);
        let request_id = "req-456";
        let mut headers = HashMap::new();
        headers.insert("test-header".to_string(), "test-value".to_string());
        let payload = json!({ "test": "payload" });

        // UniversalInstrumentationStart arrives first - should buffer
        let result = buffer
            .pair_universal_instrumentation_start_with_request_id(request_id, &headers, &payload);
        assert!(
            !result,
            "Should return false when Invoke hasn't happened yet"
        );
        assert_eq!(
            buffer.universal_instrumentation_start_events_with_id.len(),
            1,
            "Should buffer the event"
        );

        // Verify buffered event has correct request_id
        let buffered = buffer.take_universal_instrumentation_start_for_request_id(request_id);
        assert!(
            buffered.is_some(),
            "Should find buffered event by request_id"
        );
        assert_eq!(buffered.unwrap().request_id, request_id);
    }

    #[test]
    fn test_managed_instance_concurrent_invocations() {
        // Test case: Multiple concurrent invocations with interleaved events
        let mut buffer = ContextBuffer::with_capacity(10);
        let mut headers_a = HashMap::new();
        headers_a.insert("req".to_string(), "A".to_string());
        let mut headers_b = HashMap::new();
        headers_b.insert("req".to_string(), "B".to_string());
        let payload_a = json!({ "data": "A" });
        let payload_b = json!({ "data": "B" });

        // Scenario: A and B invocations interleaved
        // 1. UniversalInstrumentationStart(A) arrives
        let result_a = buffer
            .pair_universal_instrumentation_start_with_request_id("req-A", &headers_a, &payload_a);
        assert!(!result_a, "A should be buffered");

        // 2. UniversalInstrumentationStart(B) arrives
        let result_b = buffer
            .pair_universal_instrumentation_start_with_request_id("req-B", &headers_b, &payload_b);
        assert!(!result_b, "B should be buffered");

        // 3. Invoke(B) arrives - should pair with B's buffered event
        let buffered_b = buffer.take_universal_instrumentation_start_for_request_id("req-B");
        assert!(buffered_b.is_some(), "Should find B's event");
        assert_eq!(buffered_b.as_ref().unwrap().request_id, "req-B");
        assert_eq!(
            buffered_b.as_ref().unwrap().headers.get("req"),
            Some(&"B".to_string())
        );

        // 4. Invoke(A) arrives - should pair with A's buffered event
        let buffered_a = buffer.take_universal_instrumentation_start_for_request_id("req-A");
        assert!(buffered_a.is_some(), "Should find A's event");
        assert_eq!(buffered_a.as_ref().unwrap().request_id, "req-A");
        assert_eq!(
            buffered_a.as_ref().unwrap().headers.get("req"),
            Some(&"A".to_string())
        );

        // Verify no cross-contamination occurred
        assert_eq!(
            buffer.universal_instrumentation_start_events_with_id.len(),
            0,
            "All events should be paired"
        );
    }

    #[test]
    fn test_managed_instance_universal_instrumentation_end_runtime_done_first() {
        // Test case: PlatformRuntimeDone arrives before UniversalInstrumentationEnd
        let mut buffer = ContextBuffer::with_capacity(10);
        let request_id = "req-789";
        let mut headers = HashMap::new();
        headers.insert("status".to_string(), "success".to_string());
        let payload = json!({ "response": "data" });

        // Simulate PlatformRuntimeDone arriving first
        buffer
            .platform_runtime_done_events_request_ids
            .push_back(request_id.to_string());

        // UniversalInstrumentationEnd arrives - should pair immediately
        let result = buffer
            .pair_universal_instrumentation_end_with_request_id(request_id, &headers, &payload);
        assert!(
            result,
            "Should return true when PlatformRuntimeDone already happened"
        );
        assert_eq!(
            buffer.platform_runtime_done_events_request_ids.len(),
            0,
            "Should remove paired RuntimeDone event"
        );
        assert_eq!(
            buffer.universal_instrumentation_end_events_with_id.len(),
            0,
            "Should not buffer when immediate pairing occurs"
        );
    }

    #[test]
    fn test_managed_instance_universal_instrumentation_end_buffering() {
        // Test case: UniversalInstrumentationEnd arrives before PlatformRuntimeDone
        let mut buffer = ContextBuffer::with_capacity(10);
        let request_id = "req-abc";
        let mut headers = HashMap::new();
        headers.insert("status".to_string(), "success".to_string());
        let payload = json!({ "response": "data" });

        // UniversalInstrumentationEnd arrives first - should buffer
        let result = buffer
            .pair_universal_instrumentation_end_with_request_id(request_id, &headers, &payload);
        assert!(
            !result,
            "Should return false when RuntimeDone hasn't happened yet"
        );
        assert_eq!(
            buffer.universal_instrumentation_end_events_with_id.len(),
            1,
            "Should buffer the event"
        );

        // Verify buffered event has correct request_id
        let buffered = buffer.take_universal_instrumentation_end_for_request_id(request_id);
        assert!(
            buffered.is_some(),
            "Should find buffered event by request_id"
        );
        assert_eq!(buffered.unwrap().request_id, request_id);
    }

    #[test]
    fn test_managed_instance_no_cross_contamination() {
        // Test case: Ensure request_id matching prevents cross-contamination
        let mut buffer = ContextBuffer::with_capacity(10);
        let mut headers_1 = HashMap::new();
        headers_1.insert("id".to_string(), "1".to_string());
        let mut headers_2 = HashMap::new();
        headers_2.insert("id".to_string(), "2".to_string());
        let payload_1 = json!({ "data": 1 });
        let payload_2 = json!({ "data": 2 });

        // Buffer events for two different requests
        buffer
            .pair_universal_instrumentation_start_with_request_id("req-1", &headers_1, &payload_1);
        buffer
            .pair_universal_instrumentation_start_with_request_id("req-2", &headers_2, &payload_2);

        // Retrieve request-2's event - should NOT get request-1's event
        let result_2 = buffer.take_universal_instrumentation_start_for_request_id("req-2");
        assert!(result_2.is_some());
        assert_eq!(result_2.as_ref().unwrap().request_id, "req-2");
        assert_eq!(
            result_2.as_ref().unwrap().headers.get("id"),
            Some(&"2".to_string()),
            "Should get correct event for req-2"
        );

        // Request-1's event should still be buffered
        let result_1 = buffer.take_universal_instrumentation_start_for_request_id("req-1");
        assert!(result_1.is_some());
        assert_eq!(result_1.as_ref().unwrap().request_id, "req-1");
        assert_eq!(
            result_1.as_ref().unwrap().headers.get("id"),
            Some(&"1".to_string()),
            "Should get correct event for req-1"
        );
    }

    #[test]
    fn test_managed_instance_pop_nonexistent_request() {
        // Test case: Attempting to pop an event that doesn't exist
        let mut buffer = ContextBuffer::with_capacity(10);
        let result = buffer.take_universal_instrumentation_start_for_request_id("nonexistent");
        assert!(
            result.is_none(),
            "Should return None for nonexistent request_id"
        );

        let result_end = buffer.take_universal_instrumentation_end_for_request_id("nonexistent");
        assert!(
            result_end.is_none(),
            "Should return None for nonexistent request_id"
        );
    }
}
