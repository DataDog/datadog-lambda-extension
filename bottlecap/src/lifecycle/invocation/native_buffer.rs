//! Request-addressed joins between native spans and Lambda platform telemetry.

use super::{
    context::Context,
    native::{Identity, InvocationUpdate, Phase},
};
use libdd_trace_protobuf::pb::Span;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const MAX_INVOCATIONS: usize = 1024;
const ACTIVE_RETENTION: Duration = Duration::from_secs(30 * 60);
const COMPLETED_RETENTION: Duration = Duration::from_secs(120);

// Independent acknowledgments can arrive in any order.
#[allow(clippy::struct_excessive_bools)]
pub struct NativeContext {
    pub identity: Identity,
    pub complete: bool,
    pub error: bool,
    pub error_counted: bool,
    pub fallback_sent: bool,
    platform: Option<Context>,
    span: Option<(Span, bool)>,
    span_sent: bool,
    telemetry_sent: bool,
    reported: bool,
    updated: Instant,
}

#[derive(Default)]
pub struct NativeInvocations {
    contexts: HashMap<String, NativeContext>,
}

#[derive(Default)]
pub struct ReadySpans {
    pub invocation: Option<(Span, bool)>,
    pub telemetry: Option<Span>,
}

impl NativeInvocations {
    pub fn register(&mut self, update: &InvocationUpdate) -> Result<(), &'static str> {
        let identity = update.identity()?;
        self.prune(Instant::now());
        if let Some(context) = self.contexts.get_mut(&identity.request_id) {
            if context.identity.trace_id != identity.trace_id
                || context.identity.span_id != identity.span_id
                || context.identity.parent_id != identity.parent_id
            {
                return Err("conflicting invocation identity");
            }
            if update.phase == Phase::Complete {
                context.complete = true;
                context.error |= update.error;
                context.identity.sampling_priority = identity.sampling_priority;
            }
            context.updated = Instant::now();
            return Ok(());
        }
        if self.contexts.len() >= MAX_INVOCATIONS {
            return Err("native invocation capacity exceeded");
        }
        self.contexts.insert(
            identity.request_id.clone(),
            NativeContext {
                identity,
                complete: update.phase == Phase::Complete,
                error: update.error,
                error_counted: false,
                fallback_sent: false,
                platform: None,
                span: None,
                span_sent: false,
                telemetry_sent: false,
                reported: false,
                updated: Instant::now(),
            },
        );
        Ok(())
    }

    /// Returns true once for an application or platform error on a native invocation.
    pub fn record_error(&mut self, request_id: &str) -> bool {
        let Some(native) = self.contexts.get_mut(request_id) else {
            return false;
        };
        native.error = true;
        if native.error_counted {
            return false;
        }
        native.error_counted = true;
        true
    }

    #[must_use]
    pub fn get(&self, request_id: &str) -> Option<&NativeContext> {
        self.contexts.get(request_id)
    }

    pub fn get_mut(&mut self, request_id: &str) -> Option<&mut NativeContext> {
        self.contexts.get_mut(request_id)
    }

    pub fn platform_done(&mut self, context: Context) {
        if let Some(native) = self.contexts.get_mut(&context.request_id) {
            native.platform = Some(context);
            native.updated = Instant::now();
        }
    }

    pub fn receive_span(&mut self, span: Span, client_computed_stats: bool) -> bool {
        let Some(request_id) = span.meta.get("request_id") else {
            return false;
        };
        let Some(context) = self.contexts.get_mut(request_id) else {
            return false;
        };
        #[allow(clippy::cast_possible_truncation)]
        let trace_id = context.identity.trace_id as u64;
        if span.name != "aws.lambda"
            || span.span_id != context.identity.span_id
            || span.trace_id != trace_id
        {
            return false;
        }
        if !context.span_sent && !context.fallback_sent {
            context.span = Some((span, client_computed_stats));
        }
        context.updated = Instant::now();
        true
    }

    pub fn ready(&mut self, request_id: &str) -> ReadySpans {
        let Some(native) = self.contexts.get_mut(request_id) else {
            return ReadySpans::default();
        };
        let Some(platform) = native.platform.as_ref() else {
            return ReadySpans::default();
        };
        let mut ready = ReadySpans::default();
        if !native.span_sent
            && !native.fallback_sent
            && let Some((mut span, stats)) = native.span.take()
        {
            // Native duration matches the tracer's stats; platform duration remains a metric.
            span.meta.extend(platform.invocation_span.meta.clone());
            span.metrics.insert(
                "aws.lambda.runtime_duration".into(),
                platform.runtime_duration_ms,
            );
            span.error |= platform.invocation_span.error;
            ready.invocation = Some((span, stats));
            native.span_sent = true;
        }
        if !native.span_sent
            && !native.fallback_sent
            && !native.complete
            && platform.invocation_span.error != 0
        {
            let mut span = platform.invocation_span.clone();
            native.identity.apply(&mut span);
            span.meta
                .insert("_dd.lambda.native_fallback".into(), "true".into());
            ready.invocation = Some((span, false));
            native.fallback_sent = true;
        }
        if !native.telemetry_sent {
            if let Some(mut span) = platform
                .snapstart_restore_span
                .clone()
                .or_else(|| platform.cold_start_span.clone())
            {
                #[allow(clippy::cast_possible_truncation)]
                {
                    span.trace_id = native.identity.trace_id as u64;
                }
                span.parent_id = native.identity.parent_id;
                let high = native.identity.trace_id >> 64;
                if high != 0 {
                    span.meta.insert("_dd.p.tid".into(), format!("{high:016x}"));
                }
                if let Some(priority) = native.identity.sampling_priority {
                    span.metrics
                        .insert("_sampling_priority_v1".into(), f64::from(priority));
                }
                ready.telemetry = Some(span);
            }
            native.telemetry_sent = true;
        }
        ready
    }

    pub fn report(&mut self, request_id: &str) {
        if let Some(context) = self.contexts.get_mut(request_id) {
            context.reported = true;
            context.updated = Instant::now();
        }
        self.prune(Instant::now());
    }

    fn prune(&mut self, now: Instant) {
        self.contexts.retain(|_, context| {
            let retention = if context.reported {
                COMPLETED_RETENTION
            } else {
                ACTIVE_RETENTION
            };
            now.saturating_duration_since(context.updated) < retention
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(request_id: &str, span_id: u64) -> InvocationUpdate {
        InvocationUpdate {
            version: 1,
            phase: Phase::Begin,
            request_id: request_id.into(),
            trace_id: "0123456789abcdef000000000000002a".into(),
            span_id: format!("{span_id:016x}"),
            parent_id: "0000000000000007".into(),
            start_ns: 100,
            service: "function".into(),
            resource: "handler".into(),
            sampling_priority: Some(2),
            duration_ns: 0,
            error: false,
        }
    }

    fn platform(message: &InvocationUpdate) -> Context {
        let mut context = Context::from_request_id(&message.request_id);
        message
            .identity()
            .expect("valid native invocation test fixture")
            .apply(&mut context.invocation_span);
        context.runtime_done_received = true;
        context.runtime_duration_ms = 10.0;
        context
            .invocation_span
            .meta
            .insert("cold_start".into(), "true".into());
        context.cold_start_span = Some(Span {
            name: "aws.lambda.cold_start".into(),
            span_id: 999,
            ..Span::default()
        });
        context
    }

    fn span(message: &InvocationUpdate) -> Span {
        let mut span = Span {
            name: "aws.lambda".into(),
            duration: 8_000_000,
            ..Span::default()
        };
        message
            .identity()
            .expect("valid native invocation test fixture")
            .apply(&mut span);
        span
    }

    #[test]
    fn joins_both_arrival_orders_and_emits_once() {
        for telemetry_first in [true, false] {
            let message = update("request-1", 42);
            let mut buffer = NativeInvocations::default();
            buffer
                .register(&message)
                .expect("valid native invocation test fixture");
            if telemetry_first {
                buffer.platform_done(platform(&message));
            }
            assert!(buffer.receive_span(span(&message), true));
            if !telemetry_first {
                assert!(buffer.ready("request-1").invocation.is_none());
                buffer.platform_done(platform(&message));
            }
            let ready = buffer.ready("request-1");
            let (invocation, stats) = ready
                .invocation
                .expect("valid native invocation test fixture");
            assert!(stats);
            assert_eq!(invocation.duration, 8_000_000);
            assert_eq!(invocation.meta["cold_start"], "true");
            assert!(
                (invocation.metrics["aws.lambda.runtime_duration"] - 10.0).abs() < f64::EPSILON
            );
            let cold = ready
                .telemetry
                .expect("valid native invocation test fixture");
            assert_eq!(cold.trace_id, 42);
            assert_eq!(cold.parent_id, 7);
            assert_eq!(cold.meta["_dd.p.tid"], "0123456789abcdef");
            assert!(buffer.receive_span(span(&message), true));
            assert!(buffer.ready("request-1").invocation.is_none());
            assert!(buffer.ready("request-1").telemetry.is_none());
        }
    }

    #[test]
    fn report_keeps_late_intake_and_concurrent_requests_separate() {
        let a = update("a", 10);
        let b = update("b", 20);
        let mut buffer = NativeInvocations::default();
        for message in [&a, &b] {
            buffer
                .register(message)
                .expect("valid native invocation test fixture");
            buffer.platform_done(platform(message));
            buffer.report(&message.request_id);
        }
        assert!(buffer.receive_span(span(&b), false));
        assert_eq!(
            buffer
                .ready("b")
                .invocation
                .expect("valid native invocation test fixture")
                .0
                .span_id,
            20
        );
        assert!(buffer.ready("a").invocation.is_none());
        assert!(buffer.receive_span(span(&a), false));
        assert_eq!(
            buffer
                .ready("a")
                .invocation
                .expect("valid native invocation test fixture")
                .0
                .span_id,
            10
        );
        buffer.prune(Instant::now() + COMPLETED_RETENTION);
        assert!(buffer.contexts.is_empty());
    }

    #[test]
    fn duplicates_are_idempotent_and_conflicting_identity_is_rejected() {
        let message = update("a", 10);
        let mut buffer = NativeInvocations::default();
        buffer
            .register(&message)
            .expect("valid native invocation test fixture");
        buffer
            .register(&message)
            .expect("valid native invocation test fixture");
        assert!(buffer.register(&update("a", 11)).is_err());
        let mut complete = message.clone();
        complete.phase = Phase::Complete;
        complete.error = true;
        buffer
            .register(&complete)
            .expect("valid native invocation test fixture");
        buffer
            .register(&message)
            .expect("valid native invocation test fixture");
        assert!(
            buffer
                .get("a")
                .expect("valid native invocation test fixture")
                .complete
        );
        assert!(
            buffer
                .get("a")
                .expect("valid native invocation test fixture")
                .error
        );
    }

    #[test]
    fn telemetry_can_be_sent_when_native_trace_is_sampled_out() {
        let message = update("a", 10);
        let mut buffer = NativeInvocations::default();
        buffer
            .register(&message)
            .expect("valid native invocation test fixture");
        let mut context = platform(&message);
        context.snapstart_restore_span = Some(Span {
            name: "aws.lambda.snapstart_restore".into(),
            ..Span::default()
        });
        buffer.platform_done(context);
        let ready = buffer.ready("a");
        assert!(ready.invocation.is_none());
        assert_eq!(
            ready
                .telemetry
                .expect("valid native invocation test fixture")
                .name,
            "aws.lambda.snapstart_restore"
        );
    }
    #[test]
    fn hard_failure_uses_registered_identity_and_suppresses_late_native_span() {
        let message = update("timeout", 55);
        let mut buffer = NativeInvocations::default();
        buffer
            .register(&message)
            .expect("valid native invocation test fixture");
        let mut context = platform(&message);
        context.invocation_span.error = 1;
        context.invocation_span.duration = 10_000_000;
        context
            .invocation_span
            .meta
            .insert("error.type".into(), "Timeout".into());
        buffer.platform_done(context);
        let (fallback, stats) = buffer
            .ready("timeout")
            .invocation
            .expect("valid native invocation test fixture");
        assert!(!stats);
        assert_eq!(fallback.span_id, 55);
        assert_eq!(fallback.parent_id, 7);
        assert_eq!(fallback.duration, 10_000_000);
        assert_eq!(fallback.meta["error.type"], "Timeout");
        assert!(buffer.receive_span(span(&message), true));
        let mut end = message.clone();
        end.phase = Phase::Complete;
        buffer
            .register(&end)
            .expect("valid native invocation test fixture");
        assert!(buffer.ready("timeout").invocation.is_none());
    }

    #[test]
    fn completed_sampled_out_error_does_not_synthesize_another_invocation() {
        let mut message = update("complete", 55);
        message.phase = Phase::Complete;
        message.error = true;
        let mut buffer = NativeInvocations::default();
        buffer
            .register(&message)
            .expect("valid native invocation test fixture");
        let mut context = platform(&message);
        context.invocation_span.error = 1;
        buffer.platform_done(context);
        assert!(buffer.ready("complete").invocation.is_none());
        assert!(buffer.record_error("complete"));
        assert!(!buffer.record_error("complete"));
        buffer
            .register(&message)
            .expect("valid native invocation test fixture");
        assert!(!buffer.record_error("complete"));
    }
}
