use std::{collections::HashMap, sync::Arc};

use crate::{
    config::{self, trace_propagation_style::TracePropagationStyle},
    traces::context::SpanContext,
};
use carrier::{Extractor, Injector};
use libdd_trace_protobuf::pb::SpanLink;
use text_map_propagator::{
    BAGGAGE_PREFIX, DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY, DATADOG_LAST_PARENT_ID_KEY,
    TRACESTATE_KEY,
};

pub mod carrier;
pub mod error;
pub mod text_map_propagator;

pub trait Propagator {
    fn extract(&self, carrier: &dyn Extractor) -> Option<SpanContext>;
    fn inject(&self, context: SpanContext, carrier: &mut dyn Injector);
}

pub struct DatadogCompositePropagator {
    propagators: Vec<Box<dyn Propagator + Send + Sync>>,
    config: Arc<config::Config>,
}

impl Propagator for DatadogCompositePropagator {
    fn extract(&self, carrier: &dyn Extractor) -> Option<SpanContext> {
        if self.config.trace_propagation_extract_first {
            for propagator in &self.propagators {
                let context = propagator.extract(carrier);

                if let Some(mut context) = context {
                    if self.config.trace_propagation_http_baggage_enabled {
                        Self::attach_baggage(&mut context, carrier);
                    }
                    return Some(context);
                }
            }
        }

        let (contexts, styles) = self.extract_available_contexts(carrier);
        if contexts.is_empty() {
            return None;
        }

        let mut context = Self::resolve_contexts(contexts, styles, carrier);
        if self.config.trace_propagation_http_baggage_enabled {
            Self::attach_baggage(&mut context, carrier);
        }

        Some(context)
    }

    fn inject(&self, _context: SpanContext, _carrier: &mut dyn Injector) {
        todo!()
    }
}

impl DatadogCompositePropagator {
    #[must_use]
    pub fn new(config: Arc<config::Config>) -> Self {
        let propagators: Vec<Box<dyn Propagator + Send + Sync>> = config
            .trace_propagation_style_extract
            .iter()
            .filter_map(|style| match style {
                TracePropagationStyle::Datadog => {
                    Some(Box::new(text_map_propagator::DatadogHeaderPropagator)
                        as Box<dyn Propagator + Send + Sync>)
                }
                TracePropagationStyle::TraceContext => {
                    Some(Box::new(text_map_propagator::TraceContextPropagator)
                        as Box<dyn Propagator + Send + Sync>)
                }
                _ => None,
            })
            .collect();

        Self {
            propagators,
            config,
        }
    }

    fn extract_available_contexts(
        &self,
        carrier: &dyn Extractor,
    ) -> (Vec<SpanContext>, Vec<TracePropagationStyle>) {
        let mut contexts = Vec::<SpanContext>::new();
        let mut styles = Vec::<TracePropagationStyle>::new();

        for (i, propagator) in self.propagators.iter().enumerate() {
            if let Some(context) = propagator.extract(carrier) {
                contexts.push(context);
                styles.push(self.config.trace_propagation_style_extract[i]);
            }
        }

        (contexts, styles)
    }

    fn resolve_contexts(
        contexts: Vec<SpanContext>,
        styles: Vec<TracePropagationStyle>,
        _carrier: &dyn Extractor,
    ) -> SpanContext {
        let mut primary_context = contexts[0].clone();
        let mut links = Vec::<SpanLink>::new();

        let mut i = 1;
        for context in contexts.iter().skip(1) {
            let style = styles[i];

            if context.span_id != 0
                && context.trace_id != 0
                && context.trace_id != primary_context.trace_id
            {
                let sampling = context.sampling.unwrap_or_default().priority.unwrap_or(0);
                let tracestate: Option<String> = match style {
                    TracePropagationStyle::TraceContext => {
                        context.tags.get(TRACESTATE_KEY).cloned()
                    }
                    _ => None,
                };
                let attributes = HashMap::from([
                    ("reason".to_string(), "terminated_context".to_string()),
                    ("context_headers".to_string(), style.to_string()),
                ]);
                let trace_id_high_str = context
                    .tags
                    .get(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY)
                    .cloned()
                    .unwrap_or_default();
                let trace_ig_high = u64::from_str_radix(&trace_id_high_str, 16).unwrap_or_default();

                links.push(SpanLink {
                    trace_id: context.trace_id,
                    trace_id_high: trace_ig_high,
                    span_id: context.span_id,
                    flags: u32::from(sampling > 0),
                    tracestate: tracestate.unwrap_or_default(),
                    attributes,
                });
            } else if style == TracePropagationStyle::TraceContext {
                if let Some(tracestate) = context.tags.get(TRACESTATE_KEY) {
                    primary_context
                        .tags
                        .insert(TRACESTATE_KEY.to_string(), tracestate.clone());
                }

                if primary_context.trace_id == context.trace_id
                    && primary_context.span_id != context.span_id
                {
                    let mut dd_context: Option<SpanContext> = None;
                    if styles.contains(&TracePropagationStyle::Datadog) {
                        let position = styles
                            .iter()
                            .position(|&s| s == TracePropagationStyle::Datadog)
                            .unwrap_or_default();
                        dd_context = contexts.get(position).cloned();
                    }

                    if let Some(parent_id) = context.tags.get(DATADOG_LAST_PARENT_ID_KEY) {
                        primary_context
                            .tags
                            .insert(DATADOG_LAST_PARENT_ID_KEY.to_string(), parent_id.clone());
                    } else if let Some(sc) = dd_context {
                        primary_context.tags.insert(
                            DATADOG_LAST_PARENT_ID_KEY.to_string(),
                            format!("{:016x}", sc.span_id),
                        );
                    }

                    primary_context.span_id = context.span_id;
                }
            }

            i += 1;
        }

        primary_context.links = links;

        primary_context
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
    use std::sync::LazyLock;
    use std::vec;

    use crate::traces::context::Sampling;

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
    static VALID_TRACECONTEXT_HEADERS_RUM_NO_SAMPLING_DECISION: LazyLock<HashMap<String, String>> =
        LazyLock::new(|| {
            HashMap::from([
                (
                    "traceparent".to_string(),
                    format!("00-{}-00f067aa0ba902b7-00", *TRACE_ID_HEX),
                ),
                ("tracestate".to_string(), "dd=o:rum".to_string()),
            ])
        });
    static VALID_TRACECONTEXT_HEADERS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        HashMap::from([
            (
                "traceparent".to_string(),
                format!("00-{}-00f067aa0ba902b7-01", *TRACE_ID_HEX),
            ),
            (
                "tracestate".to_string(),
                "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMz".to_string(),
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
    static VALID_DATADOG_HEADERS_NO_PRIORITY: LazyLock<HashMap<String, String>> =
        LazyLock::new(|| {
            HashMap::from([
                (
                    "x-datadog-trace-id".to_string(),
                    "13088165645273925489".to_string(),
                ),
                ("x-datadog-parent-id".to_string(), "5678".to_string()),
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
    static INVALID_DATADOG_HEADERS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        HashMap::from([
            (
                "x-datadog-trace-id".to_string(),
                "13088165645273925489".to_string(),
            ),
            ("x-datadog-parent-id".to_string(), "parent_id".to_string()),
            (
                "x-datadog-sampling-priority".to_string(),
                "sample".to_string(),
            ),
        ])
    });

    // Fixtures
    static ALL_VALID_HEADERS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        let mut h = HashMap::new();
        h.extend(VALID_DATADOG_HEADERS.clone());
        h.extend(VALID_TRACECONTEXT_HEADERS.clone());
        // todo: add b3
        h
    });
    static DATADOG_TRACECONTEXT_MATCHING_TRACE_ID_HEADERS: LazyLock<HashMap<String, String>> =
        LazyLock::new(|| {
            let mut h = HashMap::new();
            h.extend(VALID_DATADOG_HEADERS_MATCHING_TRACE_CONTEXT_VALID_TRACE_ID.clone());
            // We use 64-bit traceparent trace id value here so it can match for
            // both 128-bit enabled and disabled
            h.extend(VALID_TRACECONTEXT_HEADERS_VALID_64_BIT_TRACE_ID.clone());
            h
        });
    // Edge cases
    #[allow(dead_code)]
    static ALL_HEADERS_CHAOTIC_1: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        let mut h = HashMap::new();
        h.extend(VALID_DATADOG_HEADERS_MATCHING_TRACE_CONTEXT_VALID_TRACE_ID.clone());
        h.extend(VALID_TRACECONTEXT_HEADERS_VALID_64_BIT_TRACE_ID.clone());
        // todo: add b3
        h
    });
    static ALL_HEADERS_CHAOTIC_2: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
        let mut h = HashMap::new();
        h.extend(VALID_DATADOG_HEADERS.clone());
        h.extend(VALID_TRACECONTEXT_HEADERS_VALID_64_BIT_TRACE_ID.clone());
        // todo: add b3
        h
    });
    static NO_TRACESTATE_SUPPORT_NOT_MATCHING_TRACE_ID: LazyLock<HashMap<String, String>> =
        LazyLock::new(|| {
            let mut h = HashMap::new();
            h.extend(VALID_DATADOG_HEADERS.clone());
            h.extend(VALID_TRACECONTEXT_HEADERS_RUM_NO_SAMPLING_DECISION.clone());
            h
        });

    macro_rules! test_propagation_extract {
        ($($name:ident: $value:expr,)*) => {
            $(
                #[test]
                fn $name() {
                    let (styles, carrier, expected) = $value;
                    let mut config = config::Config::default();
                    config.trace_propagation_style_extract = vec![TracePropagationStyle::Datadog, TracePropagationStyle::TraceContext];
                    if let Some(s) = styles {
                        config.trace_propagation_style_extract.clone_from(&s);
                    }

                    let propagator = DatadogCompositePropagator::new(Arc::new(config));

                    let context = propagator.extract(&carrier).unwrap_or_default();

                    assert_eq!(context, expected);
                }
            )*
        }
    }

    test_propagation_extract! {
        // Datadog Headers
        valid_datadog_default: (
            None,
            VALID_DATADOG_HEADERS.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![],
            }
        ),
        valid_datadog_no_priority: (
            None,
            VALID_DATADOG_HEADERS_NO_PRIORITY.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(2),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![],
            },
        ),
        invalid_datadog: (
            Some(vec![TracePropagationStyle::Datadog]),
            INVALID_DATADOG_HEADERS.clone(),
            SpanContext::default(),
        ),
        valid_datadog_explicit_style: (
            Some(vec![TracePropagationStyle::Datadog]),
            VALID_DATADOG_HEADERS.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![],
            },
        ),
        invalid_datadog_negative_trace_id: (
            Some(vec![TracePropagationStyle::Datadog]),
            HashMap::from([
                (
                    "x-datadog-trace-id".to_string(),
                    "-1".to_string(),
                ),
                ("x-datadog-parent-id".to_string(), "5678".to_string(),),
                ("x-datadog-sampling-priority".to_string(), "1".to_string()),
                ("x-datadog-origin".to_string(), "synthetics".to_string()),
            ]),
            SpanContext::default(),
        ),
        valid_datadog_no_datadog_style: (
            Some(vec![TracePropagationStyle::TraceContext]),
            VALID_DATADOG_HEADERS.clone(),
            SpanContext::default(),
        ),
        // TraceContext Headers
        valid_tracecontext_simple: (
            Some(vec![TracePropagationStyle::TraceContext]),
            VALID_TRACECONTEXT_HEADERS_BASIC.clone(),
            SpanContext {
                trace_id: 7_277_407_061_855_694_839,
                span_id: 67_667_974_448_284_343,
                sampling: Some(Sampling {
                    priority: Some(2),
                    mechanism: None,
                }),
                origin: Some("rum".to_string()),
                tags: HashMap::from([
                    ("tracestate".to_string(), "dd=p:00f067aa0ba902b7;s:2;o:rum".to_string()),
                    ("_dd.p.tid".to_string(), "9291375655657946024".to_string()),
                    ("traceparent".to_string(), "00-80f198ee56343ba864fe8b2a57d3eff7-00f067aa0ba902b7-01".to_string()),
                    ("_dd.parent_id".to_string(), "00f067aa0ba902b7".to_string()),
                ]),
                links: vec![],
            }
        ),
        valid_tracecontext_rum_no_sampling_decision: (
            Some(vec![TracePropagationStyle::TraceContext]),
            VALID_TRACECONTEXT_HEADERS_RUM_NO_SAMPLING_DECISION.clone(),
            SpanContext {
                trace_id: 7_277_407_061_855_694_839,
                span_id: 67_667_974_448_284_343,
                sampling: Some(Sampling {
                    priority: Some(0),
                    mechanism: None,
                }),
                origin: Some("rum".to_string()),
                tags: HashMap::from([
                    ("_dd.p.tid".to_string(), "9291375655657946024".to_string()),
                    ("tracestate".to_string(), "dd=o:rum".to_string()),
                    ("traceparent".to_string(), "00-80f198ee56343ba864fe8b2a57d3eff7-00f067aa0ba902b7-00".to_string()),
                ]),
                links: vec![],
            }
        ),
        // B3 Headers
        // todo: all of them
        // B3 single Headers
        // todo: all of them
        // All Headers
        valid_all_headers: (
            None,
            ALL_VALID_HEADERS.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![
                    SpanLink {
                        trace_id: 7_277_407_061_855_694_839,
                        trace_id_high: 0,
                        span_id: 67_667_974_448_284_343,
                        flags: 1,
                        tracestate: "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMz".to_string(),
                        attributes: HashMap::from([
                            ("reason".to_string(), "terminated_context".to_string()),
                            ("context_headers".to_string(), "tracecontext".to_string()),
                        ]),
                    }
                ],
            },
        ),
        valid_all_headers_all_styles: (
            Some(vec![TracePropagationStyle::Datadog, TracePropagationStyle::TraceContext]),
            ALL_VALID_HEADERS.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![
                    SpanLink {
                        trace_id: 7_277_407_061_855_694_839,
                        trace_id_high: 0,
                        span_id: 67_667_974_448_284_343,
                        flags: 1,
                        tracestate: "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMz".to_string(),
                        attributes: HashMap::from([
                            ("reason".to_string(), "terminated_context".to_string()),
                            ("context_headers".to_string(), "tracecontext".to_string()),
                        ]),
                    }
                    // todo: b3 span links
                ],
            },
        ),
        valid_all_headers_datadog_style: (
            Some(vec![TracePropagationStyle::Datadog]),
            ALL_VALID_HEADERS.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![]
            },
        ),
        // todo: valid_all_headers_b3_style
        // todo: valid_all_headers_both_b3_styles
        // todo: valid_all_headers_b3_single_style
        none_style: (
            Some(vec![TracePropagationStyle::None]),
            ALL_VALID_HEADERS.clone(),
            SpanContext::default(),
        ),
        valid_style_and_none_still_extracts: (
            Some(vec![TracePropagationStyle::Datadog, TracePropagationStyle::None]),
            ALL_VALID_HEADERS.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![],
            }
        ),
        // Order matters
        // todo: order_matters_b3_single_header_first
        // todo: order_matters_b3_first
        // todo: order_matters_b3_second_no_datadog_headers
        // Tracestate is still added when TraceContext style comes later and matches
        // first style's `trace_id`
        additional_tracestate_support_when_present_and_matches_first_style_trace_id: (
            Some(vec![TracePropagationStyle::Datadog, TracePropagationStyle::TraceContext]),
            DATADOG_TRACECONTEXT_MATCHING_TRACE_ID_HEADERS.clone(),
            SpanContext {
                trace_id: 7_277_407_061_855_694_839,
                span_id: 67_667_974_448_284_343,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string()),
                    ("_dd.parent_id".to_string(), "000000000000162e".to_string()),
                    (TRACESTATE_KEY.to_string(), "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMzE".to_string())
                ]),
                links: vec![],
            }
        ),
        // Tracestate is not added when TraceContext style comes later and does not
        // match first style's `trace_id`
        no_additional_tracestate_support_when_present_and_trace_id_does_not_match: (
            Some(vec![TracePropagationStyle::Datadog, TracePropagationStyle::TraceContext]),
            NO_TRACESTATE_SUPPORT_NOT_MATCHING_TRACE_ID.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![
                    SpanLink {
                        trace_id: 7_277_407_061_855_694_839,
                        trace_id_high: 0,
                        span_id: 67_667_974_448_284_343,
                        flags: 0,
                        tracestate: "dd=o:rum".to_string(),
                        attributes: HashMap::from([
                            ("reason".to_string(), "terminated_context".to_string()),
                            ("context_headers".to_string(), "tracecontext".to_string()),
                        ]),
                    }
                ],
            }
        ),
        valid_all_headers_no_style: (
            Some(vec![]),
            ALL_VALID_HEADERS.clone(),
            SpanContext::default(),
        ),
        datadog_tracecontext_conflicting_span_ids: (
            Some(vec![TracePropagationStyle::Datadog, TracePropagationStyle::TraceContext]),
            HashMap::from([
                (
                    "x-datadog-trace-id".to_string(),
                    "9291375655657946024".to_string(),
                ),
                ("x-datadog-parent-id".to_string(), "15".to_string(),),
                ("traceparent".to_string(), "00-000000000000000080f198ee56343ba8-000000000000000a-01".to_string()),
            ]),
            SpanContext {
                trace_id: 9_291_375_655_657_946_024,
                span_id: 10,
                sampling: Some(Sampling {
                    priority: Some(2),
                    mechanism: None,
                }),
                origin: None,
                tags: HashMap::from([
                    ("_dd.parent_id".to_string(), "000000000000000f".to_string()),
                    ("_dd.p.dm".to_string(), "-3".to_string()),
                ]),
                links: vec![],
            }
        ),
        // todo: all_headers_all_styles_tracecontext_t_id_match_no_span_link
        all_headers_all_styles_do_not_create_span_link_for_context_w_out_span_id: (
            Some(vec![TracePropagationStyle::TraceContext, TracePropagationStyle::Datadog]),
            ALL_HEADERS_CHAOTIC_2.clone(),
            SpanContext {
                trace_id: 7_277_407_061_855_694_839,
                span_id: 67_667_974_448_284_343,
                sampling: Some(Sampling {
                    priority: Some(2),
                    mechanism: None,
                }),
                origin: Some("rum".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-4".to_string()),
                    ("_dd.p.tid".to_string(), "0".to_string()),
                    ("_dd.p.usr.id".to_string(), "baz64".to_string()),
                    ("traceparent".to_string(), "00-000000000000000064fe8b2a57d3eff7-00f067aa0ba902b7-01".to_string()),
                    ("tracestate".to_string(), "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMzE".to_string()),
                ]),
                links: vec![
                    SpanLink {
                        trace_id: 13_088_165_645_273_925_489,
                        trace_id_high: 0,
                        span_id: 5678,
                        flags: 1,
                        tracestate: String::new(),
                        attributes: HashMap::from([
                            ("reason".to_string(), "terminated_context".to_string()),
                            ("context_headers".to_string(), "datadog".to_string()),
                        ]),
                    }
                ],
            }
        ),
        all_headers_all_styles_tracecontext_primary_only_datadog_t_id_diff: (
            Some(vec![TracePropagationStyle::TraceContext, TracePropagationStyle::Datadog]),
            ALL_VALID_HEADERS.clone(),
            SpanContext {
                trace_id: 7_277_407_061_855_694_839,
                span_id: 67_667_974_448_284_343,
                sampling: Some(Sampling {
                    priority: Some(2),
                    mechanism: None,
                }),
                origin: Some("rum".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-4".to_string()),
                    ("_dd.p.tid".to_string(), "9291375655657946024".to_string()),
                    ("_dd.p.usr.id".to_string(), "baz64".to_string()),
                    ("traceparent".to_string(), "00-80f198ee56343ba864fe8b2a57d3eff7-00f067aa0ba902b7-01".to_string()),
                    ("tracestate".to_string(), "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMz".to_string()),
                ]),
                links: vec![
                    SpanLink {
                        trace_id: 13_088_165_645_273_925_489,
                        trace_id_high: 0,
                        span_id: 5678,
                        flags: 1,
                        tracestate: String::new(),
                        attributes: HashMap::from([
                            ("reason".to_string(), "terminated_context".to_string()),
                            ("context_headers".to_string(), "datadog".to_string()),
                        ]),
                    }
                ],
            }
        ),
        // todo: fix this test
        all_headers_all_styles_datadog_primary_only_datadog_t_id_diff: (
            Some(vec![TracePropagationStyle::Datadog, TracePropagationStyle::TraceContext]),
            ALL_VALID_HEADERS.clone(),
            SpanContext {
                trace_id: 13_088_165_645_273_925_489,
                span_id: 5678,
                sampling: Some(Sampling {
                    priority: Some(1),
                    mechanism: None,
                }),
                origin: Some("synthetics".to_string()),
                tags: HashMap::from([
                    ("_dd.p.dm".to_string(), "-3".to_string())
                ]),
                links: vec![
                    SpanLink {
                        trace_id: 7_277_407_061_855_694_839,
                        // this should be `9291375655657946024` not `0`, but we don't have this data
                        // with the current definition of `SpanContext`
                        trace_id_high: 0,
                        span_id: 67_667_974_448_284_343,
                        flags: 1,
                        tracestate: "dd=s:2;o:rum;t.dm:-4;t.usr.id:baz64,congo=t61rcWkgMz".to_string(),
                        attributes: HashMap::from([
                            ("reason".to_string(), "terminated_context".to_string()),
                            ("context_headers".to_string(), "tracecontext".to_string()),
                        ]),
                    }
                ],
            }
        ),
        // todo: datadog_primary_match_tracecontext_dif_from_b3_b3multi_invalid
    }

    #[test]
    fn test_new_filter_propagators() {
        let config = config::Config {
            trace_propagation_style_extract: vec![
                TracePropagationStyle::Datadog,
                TracePropagationStyle::TraceContext,
                TracePropagationStyle::B3,
                TracePropagationStyle::B3Multi,
            ],
            ..Default::default()
        };

        let propagator = DatadogCompositePropagator::new(Arc::new(config));

        assert_eq!(propagator.propagators.len(), 2);
    }

    #[test]
    fn test_new_no_propagators() {
        let config = config::Config {
            trace_propagation_style_extract: vec![TracePropagationStyle::None],
            ..Default::default()
        };
        let propagator = DatadogCompositePropagator::new(Arc::new(config));

        assert_eq!(propagator.propagators.len(), 0);
    }

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
        let (contexts, styles) = propagator.extract_available_contexts(&carrier);

        assert_eq!(contexts.len(), 2);
        assert_eq!(styles.len(), 2);
    }

    #[test]
    fn test_extract_available_contexts_no_contexts() {
        let config = config::Config {
            trace_propagation_style_extract: vec![TracePropagationStyle::Datadog],
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
        ]);
        let (contexts, styles) = propagator.extract_available_contexts(&carrier);

        assert_eq!(contexts.len(), 0);
        assert_eq!(styles.len(), 0);
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
