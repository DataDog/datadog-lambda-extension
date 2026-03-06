use std::sync::Arc;

use crate::config;
use datadog_opentelemetry::propagation::{
    self as dd_propagation,
    carrier::Extractor,
    context::SpanContext,
};

pub mod carrier;

const BAGGAGE_PREFIX: &str = "ot-baggage-";

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
            if let Some(stripped) = key.strip_prefix(BAGGAGE_PREFIX) {
                context.tags.insert(
                    stripped.to_string(),
                    carrier.get(key).unwrap_or_default().to_string(),
                );
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    use datadog_opentelemetry::propagation::TracePropagationStyle;

    use super::*;

    fn lower_64_bits(value: u128) -> u64 {
        (value & 0xFFFF_FFFF_FFFF_FFFF) as u64
    }

    static TRACE_ID: LazyLock<u128> =
        LazyLock::new(|| 171_395_628_812_617_415_352_188_477_958_425_669_623);
    static TRACE_ID_LOWER_ORDER_BITS: LazyLock<u64> = LazyLock::new(|| lower_64_bits(*TRACE_ID));
    static TRACE_ID_HEX: LazyLock<String> =
        LazyLock::new(|| String::from("80f198ee56343ba864fe8b2a57d3eff7"));

    // TraceContext Headers
    static VALID_TRACECONTEXT_HEADERS_BASIC: LazyLock<HashMap<String, String>> =
        LazyLock::new(|| {
            HashMap::from([
                (
                    "traceparent".to_string(),
                    format!("00-{}-00f067aa0ba902b7-01", *TRACE_ID_HEX),
                ),
                (
                    "tracestate".to_string(),
                    "dd=p:00f067aa0ba902b7;s:2;o:rum".to_string(),
                ),
            ])
        });
    static VALID_TRACECONTEXT_HEADERS_VALID_64_BIT_TRACE_ID: LazyLock<HashMap<String, String>> =
        LazyLock::new(|| {
            HashMap::from([
                (
                    "traceparent".to_string(),
                    "00-000000000000000064fe8b2a57d3eff7-00f067aa0ba902b7-01".to_string(),
                ),
                (
                    "tracestate".to_string(),
                    "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMzE".to_string(),
                ),
            ])
        });

    // Datadog Headers
    static VALID_DATADOG_HEADERS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "13088165645273925489".to_string(),
            ),
            ("x-datadog-parent-id".to_string(), "5678".to_string()),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
            ("x-datadog-origin".to_string(), "synthetics".to_string()),
        ])
    });
    static VALID_DATADOG_HEADERS_MATCHING_TRACE_CONTEXT_VALID_TRACE_ID: LazyLock<
        HashMap<String, String>,
    > = LazyLock::new(|| {
        HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                TRACE_ID_LOWER_ORDER_BITS.to_string(),
            ),
            ("x-datadog-parent-id".to_string(), "5678".to_string()),
            ("x-datadog-origin".to_string(), "synthetics".to_string()),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ])
    });

    // Fixtures
    static ALL_VALID_HEADERS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        let mut h = HashMap::new();
        h.extend(VALID_DATADOG_HEADERS.clone());
        h.extend(HashMap::from([
            (
                "traceparent".to_string(),
                format!("00-{}-00f067aa0ba902b7-01", *TRACE_ID_HEX),
            ),
            (
                "tracestate".to_string(),
                "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMz".to_string(),
            ),
        ]));
        h
    });
    static DATADOG_TRACECONTEXT_MATCHING_TRACE_ID_HEADERS: LazyLock<HashMap<String, String>> =
        LazyLock::new(|| {
            let mut h = HashMap::new();
            h.extend(VALID_DATADOG_HEADERS_MATCHING_TRACE_CONTEXT_VALID_TRACE_ID.clone());
            h.extend(VALID_TRACECONTEXT_HEADERS_VALID_64_BIT_TRACE_ID.clone());
            h
        });

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
}
