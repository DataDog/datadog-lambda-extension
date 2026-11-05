//! Metadata contract for tracers that own Lambda invocation spans.

use libdd_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Begin,
    Complete,
}

/// IDs are hexadecimal strings so JSON clients cannot lose precision.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InvocationUpdate {
    pub version: u8,
    pub phase: Phase,
    pub request_id: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_id: String,
    pub start_ns: i64,
    pub service: String,
    pub resource: String,
    pub sampling_priority: Option<i32>,
    #[serde(default)]
    pub duration_ns: i64,
    #[serde(default)]
    pub error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub request_id: String,
    pub trace_id: u128,
    pub span_id: u64,
    pub parent_id: u64,
    pub start_ns: i64,
    pub service: String,
    pub resource: String,
    pub sampling_priority: Option<i32>,
}

impl InvocationUpdate {
    pub fn identity(&self) -> Result<Identity, &'static str> {
        if self.version != PROTOCOL_VERSION {
            return Err("unsupported native invocation protocol version");
        }
        if self.request_id.is_empty() || self.request_id.len() > 128 {
            return Err("invalid request id");
        }
        if self.trace_id.len() != 32 || self.span_id.len() != 16 || self.parent_id.len() != 16 {
            return Err("invalid identity width");
        }
        let trace_id = u128::from_str_radix(&self.trace_id, 16).map_err(|_| "invalid trace id")?;
        let span_id = u64::from_str_radix(&self.span_id, 16).map_err(|_| "invalid span id")?;
        let parent_id =
            u64::from_str_radix(&self.parent_id, 16).map_err(|_| "invalid parent id")?;
        if trace_id == 0 || span_id == 0 || self.start_ns <= 0 || self.duration_ns < 0 {
            return Err("invalid identity or timestamp");
        }
        if self.service.len() > 256 || self.resource.len() > 256 {
            return Err("invalid service or resource length");
        }
        if self
            .sampling_priority
            .is_some_and(|priority| !(-1..=2).contains(&priority))
        {
            return Err("invalid sampling priority");
        }
        Ok(Identity {
            request_id: self.request_id.clone(),
            trace_id,
            span_id,
            parent_id,
            start_ns: self.start_ns,
            service: self.service.clone(),
            resource: self.resource.clone(),
            sampling_priority: self.sampling_priority,
        })
    }
}

impl Identity {
    pub fn apply(&self, span: &mut Span) {
        #[allow(clippy::cast_possible_truncation)]
        {
            span.trace_id = self.trace_id as u64;
        }
        span.span_id = self.span_id;
        span.parent_id = self.parent_id;
        span.service.clone_from(&self.service);
        span.resource.clone_from(&self.resource);
        if span.start == 0 {
            span.start = self.start_ns;
        }
        span.meta
            .insert("request_id".into(), self.request_id.clone());
        let high = self.trace_id >> 64;
        if high != 0 {
            span.meta.insert("_dd.p.tid".into(), format!("{high:016x}"));
        }
        if let Some(priority) = self.sampling_priority {
            span.metrics
                .insert("_sampling_priority_v1".into(), f64::from(priority));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update() -> InvocationUpdate {
        serde_json::from_str(
            r#"{
            "version":1,"phase":"begin","request_id":"request-1",
            "trace_id":"0123456789abcdefffffffffffffffff",
            "span_id":"fffffffffffffffe","parent_id":"0000000000000000",
            "start_ns":1700000000000000000,"service":"function","resource":"handler",
            "sampling_priority":2
        }"#,
        )
        .expect("valid native invocation test fixture")
    }

    #[test]
    fn preserves_unsigned_identity_and_platform_timestamp() {
        let identity = update()
            .identity()
            .expect("valid native invocation test fixture");
        let mut span = Span {
            start: 123,
            ..Span::default()
        };
        identity.apply(&mut span);
        assert_eq!(span.trace_id, u64::MAX);
        assert_eq!(span.span_id, u64::MAX - 1);
        assert_eq!(span.meta["_dd.p.tid"], "0123456789abcdef");
        assert_eq!(span.meta["request_id"], "request-1");
        assert!((span.metrics["_sampling_priority_v1"] - 2.0).abs() < f64::EPSILON);
        assert_eq!(span.start, 123);
    }

    #[test]
    fn validates_versions_ids_and_timestamps() {
        let mut message = update();
        message.version = 2;
        assert!(message.identity().is_err());
        message = update();
        message.span_id = "0000000000000000".into();
        assert!(message.identity().is_err());
        message = update();
        message.trace_id = "00000000000000000000000000000000".into();
        assert!(message.identity().is_err());
        message = update();
        message.trace_id = "zz23456789abcdefffffffffffffffff".into();
        assert!(message.identity().is_err());
        message = update();
        message.duration_ns = -1;
        assert!(message.identity().is_err());
        message = update();
        message.sampling_priority = Some(3);
        assert!(message.identity().is_err());
    }

    #[test]
    fn complete_round_trips_without_event_payload() {
        let mut message = update();
        message.phase = Phase::Complete;
        message.duration_ns = 1_000_000;
        message.error = true;
        let json = serde_json::to_string(&message).expect("valid native invocation test fixture");
        let decoded: InvocationUpdate =
            serde_json::from_str(&json).expect("valid native invocation test fixture");
        assert_eq!(decoded.phase, Phase::Complete);
        assert_eq!(decoded.duration_ns, 1_000_000);
        assert!(decoded.error);
        assert_eq!(
            decoded
                .identity()
                .expect("valid native invocation test fixture"),
            message
                .identity()
                .expect("valid native invocation test fixture")
        );
    }
}
