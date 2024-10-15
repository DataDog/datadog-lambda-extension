use std::{collections::HashMap, sync::Arc};

use crate::{
    config::{self, trace_propagation_style::TracePropagationStyle},
    traces::context::SpanContext,
};
use carrier::{Extractor, Injector};
use datadog_trace_protobuf::pb::SpanLink;
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
    propagators: Vec<Box<dyn Propagator + 'static>>,
    config: Arc<config::Config>,
}

#[allow(clippy::never_loop)]
impl Propagator for DatadogCompositePropagator {
    fn extract(&self, carrier: &dyn Extractor) -> Option<SpanContext> {
        if self.config.trace_propagation_extract_first {
            for propagator in &self.propagators {
                let context = propagator.extract(carrier);

                if self.config.trace_propagation_http_baggage_enabled {
                    if let Some(mut context) = context {
                        Self::attach_baggage(&mut context, carrier);
                        return Some(context);
                    }
                }

                return context;
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
        let propagators: Vec<Box<dyn Propagator + 'static>> = config
            .trace_propagation_style_extract
            .iter()
            .filter_map(|style| match style {
                TracePropagationStyle::Datadog => {
                    Some(Box::new(text_map_propagator::DatadogHeaderPropagator)
                        as Box<dyn Propagator>)
                }
                TracePropagationStyle::TraceContext => {
                    Some(Box::new(text_map_propagator::TraceContextPropagator)
                        as Box<dyn Propagator>)
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
                    && primary_context.span_id == context.span_id
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
