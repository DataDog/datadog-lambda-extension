use std::collections::HashMap;
use std::sync::Arc;

use crate::config;
use datadog_opentelemetry::propagation::{
    self as dd_propagation, carrier::Extractor, context::SpanContext,
};

pub mod carrier;

const BAGGAGE_PREFIX: &str = "ot-baggage-";
const PROPAGATION_TAG_PREFIX: &str = "_dd.p.";

#[must_use]
pub fn extract_propagation_tags(tags_str: &str) -> HashMap<String, String> {
    tags_str
        .split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k.starts_with(PROPAGATION_TAG_PREFIX) {
                Some((k.to_string(), v.to_string()))
            } else {
                None
            }
        })
        .collect()
}

// Thin wrapper around dd-trace-rs's propagator to add `ot-baggage-*` header
// extraction, which is not yet supported upstream in datadog-opentelemetry.
pub struct DatadogCompositePropagator {
    inner: dd_propagation::DatadogCompositePropagator<config::Config>,
    config: Arc<config::Config>,
}

impl DatadogCompositePropagator {
    #[must_use]
    pub fn new(config: Arc<config::Config>) -> Self {
        let inner = dd_propagation::DatadogCompositePropagator::new(Arc::clone(&config));
        Self { inner, config }
    }

    pub fn extract(&self, carrier: &dyn Extractor) -> Option<SpanContext> {
        let mut context = self.inner.extract(carrier)?;

        if self.config.trace_propagation_http_baggage_enabled {
            Self::attach_baggage(&mut context, carrier);
        }

        Some(context)
    }

    fn attach_baggage(context: &mut SpanContext, carrier: &dyn Extractor) {
        let keys = carrier.keys();

        for key in keys {
            let lower = key.to_ascii_lowercase();
            if let Some(stripped) = lower.strip_prefix(BAGGAGE_PREFIX) {
                context.tags.insert(
                    stripped.to_string(),
                    carrier.get(key).unwrap_or_default().to_string(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use datadog_opentelemetry::propagation::TracePropagationStyle;

    use super::*;

    #[test]
    fn test_extract_available_contexts() {
        let config = config::Config {
            trace_propagation_style_extract: vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
            ],
            ..Default::default()
        };

        let propagator = DatadogCompositePropagator::new(Arc::new(config));

        let carrier = HashMap::from([
            (
                "traceparent".to_string(),
                "00-80f198ee56343ba864fe8b2a57d3eff7-00f067aa0ba902b7-01".to_string(),
            ),
            (
                "tracestate".to_string(),
                "dd=p:00f067aa0ba902b7;s:2;o:rum".to_string(),
            ),
            (
                "x-datadog-trace-id".to_string(),
                "7277407061855694839".to_string(),
            ),
            (
                "x-datadog-parent-id".to_string(),
                "67667974448284343".to_string(),
            ),
            ("x-datadog-sampling-priority".to_string(), "2".to_string()),
            ("x-datadog-origin".to_string(), "rum".to_string()),
            (
                "x-datadog-tags".to_string(),
                "_dd.p.test=value,_dd.p.tid=9291375655657946024,any=tag".to_string(),
            ),
        ]);
        let context = propagator.extract(&carrier);
        assert!(context.is_some());
    }

    #[test]
    fn test_attach_baggage() {
        let mut context = SpanContext::default();
        let carrier = HashMap::from([
            ("x-datadog-trace-id".to_string(), "123".to_string()),
            ("x-datadog-parent-id".to_string(), "5678".to_string()),
            ("ot-baggage-key1".to_string(), "value1".to_string()),
        ]);

        DatadogCompositePropagator::attach_baggage(&mut context, &carrier);

        assert_eq!(context.tags.len(), 1);
        assert_eq!(context.tags.get("key1").expect("Missing tag"), "value1");
    }

    #[test]
    fn test_attach_baggage_multiple_keys() {
        let mut context = SpanContext::default();
        let carrier = HashMap::from([
            ("ot-baggage-key1".to_string(), "value1".to_string()),
            ("ot-baggage-key2".to_string(), "value2".to_string()),
            ("x-datadog-trace-id".to_string(), "123".to_string()),
        ]);

        DatadogCompositePropagator::attach_baggage(&mut context, &carrier);

        assert_eq!(context.tags.len(), 2);
        assert_eq!(context.tags.get("key1").expect("Missing tag"), "value1");
        assert_eq!(context.tags.get("key2").expect("Missing tag"), "value2");
    }

    #[test]
    fn test_extract_propagation_tags() {
        let tags = extract_propagation_tags("_dd.p.tid=abc123,any=tag,_dd.p.dm=-4");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags.get("_dd.p.tid").expect("Missing tag"), "abc123");
        assert_eq!(tags.get("_dd.p.dm").expect("Missing tag"), "-4");
    }
}
