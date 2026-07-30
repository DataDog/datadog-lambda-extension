use std::collections::HashMap;
use std::sync::Arc;

use crate::config;
use datadog_opentelemetry::propagation::{
    self as dd_propagation, ExtractResult, carrier::Extractor, context::SpanContext,
    datadog::DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY,
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
    inner:
        dd_propagation::DatadogCompositePropagator<crate::config::propagation_wrapper::PropConfig>,
    config: Arc<config::Config>,
}

impl DatadogCompositePropagator {
    #[must_use]
    pub fn new(config: Arc<config::Config>) -> Self {
        let prop_cfg = crate::config::propagation_wrapper::PropConfig::new(Arc::clone(&config));
        let inner = dd_propagation::DatadogCompositePropagator::new(prop_cfg);
        Self { inner, config }
    }

    pub fn extract(&self, carrier: &dyn Extractor) -> Option<SpanContext> {
        let mut context = match self.inner.extract(carrier) {
            ExtractResult::Continue(context) => context,
            ExtractResult::Passthrough | ExtractResult::Ignore | ExtractResult::Restart(_) => {
                return None;
            }
        };

        Self::preserve_high_order_trace_id_bits(&mut context);

        if self.config.trace_propagation_http_baggage_enabled {
            Self::attach_baggage(&mut context, carrier);
        }

        Some(context)
    }

    // The b3/b3multi extractors return a full 128-bit `trace_id` without setting
    // `_dd.p.tid` (unlike the Datadog format extractor), but downstream code casts
    // `trace_id` to u64 and relies on that tag to avoid losing the high bits.
    fn preserve_high_order_trace_id_bits(context: &mut SpanContext) {
        let high = (context.trace_id >> 64) as u64;
        if high != 0
            && !context
                .tags
                .contains_key(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY)
        {
            context.tags.insert(
                DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY.to_string(),
                format!("{high:016x}"),
            );
        }
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
    fn test_extract_b3_single_preserves_high_order_trace_id_bits() {
        let config = config::Config {
            trace_propagation_style_extract: vec![TracePropagationStyle::B3SingleHeader],
            ..Default::default()
        };

        let propagator = DatadogCompositePropagator::new(Arc::new(config));

        let carrier = HashMap::from([(
            "b3".to_string(),
            "80f198ee56343ba864fe8b2a57d3eff7-e457b5a2e4d86bd1-1".to_string(),
        )]);

        let context = propagator.extract(&carrier).expect("context");
        assert_eq!(
            context.trace_id,
            0x80f1_98ee_5634_3ba8_64fe_8b2a_57d3_eff7u128
        );
        assert_eq!(
            context
                .tags
                .get("_dd.p.tid")
                .expect("Missing _dd.p.tid tag"),
            "80f198ee56343ba8"
        );
    }

    #[test]
    fn test_extract_b3_multi_preserves_high_order_trace_id_bits() {
        let config = config::Config {
            trace_propagation_style_extract: vec![TracePropagationStyle::B3Multi],
            ..Default::default()
        };

        let propagator = DatadogCompositePropagator::new(Arc::new(config));

        let carrier = HashMap::from([
            (
                "x-b3-traceid".to_string(),
                "80f198ee56343ba864fe8b2a57d3eff7".to_string(),
            ),
            ("x-b3-spanid".to_string(), "e457b5a2e4d86bd1".to_string()),
            ("x-b3-sampled".to_string(), "1".to_string()),
        ]);

        let context = propagator.extract(&carrier).expect("context");
        assert_eq!(
            context.trace_id,
            0x80f1_98ee_5634_3ba8_64fe_8b2a_57d3_eff7u128
        );
        assert_eq!(
            context
                .tags
                .get("_dd.p.tid")
                .expect("Missing _dd.p.tid tag"),
            "80f198ee56343ba8"
        );
    }

    #[test]
    fn test_extract_b3_single_64_bit_trace_id_does_not_set_tid_tag() {
        let config = config::Config {
            trace_propagation_style_extract: vec![TracePropagationStyle::B3SingleHeader],
            ..Default::default()
        };

        let propagator = DatadogCompositePropagator::new(Arc::new(config));

        let carrier = HashMap::from([(
            "b3".to_string(),
            "80f198ee56343ba8-e457b5a2e4d86bd1-1".to_string(),
        )]);

        let context = propagator.extract(&carrier).expect("context");
        assert_eq!(context.trace_id, 0x80f1_98ee_5634_3ba8u128);
        assert!(!context.tags.contains_key("_dd.p.tid"));
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
