use std::sync::Arc;

use datadog_opentelemetry::configuration::{
    TracePropagationBehaviorExtract, TracePropagationStyle as OtelTracePropagationStyle,
};
use datadog_opentelemetry::propagation::PropagationConfig;

use crate::config::{Config, TracePropagationStyle};

/// Newtype wrapper that lets us implement `PropagationConfig` for bottlecap's
/// `Config` without tripping Rust's orphan rule. Both the trait and the
/// underlying `datadog_agent_config::Config<LambdaConfig>` are foreign, so the
/// wrapper is the local type the impl can attach to. Callers that need a
/// propagator hand it an `Arc<PropConfig>` instead of an `Arc<Config>`.
#[derive(Debug, Clone)]
pub struct PropConfig {
    config: Arc<Config>,
    trace_propagation_style: Vec<OtelTracePropagationStyle>,
    trace_propagation_style_extract: Vec<OtelTracePropagationStyle>,
}

impl PropConfig {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Arc<Self> {
        Arc::new(Self {
            trace_propagation_style: config
                .trace_propagation_style
                .iter()
                .copied()
                .map(convert_trace_propagation_style)
                .collect(),
            trace_propagation_style_extract: config
                .trace_propagation_style_extract
                .iter()
                .copied()
                .map(convert_trace_propagation_style)
                .collect(),
            config,
        })
    }
}

const fn convert_trace_propagation_style(
    style: TracePropagationStyle,
) -> OtelTracePropagationStyle {
    match style {
        TracePropagationStyle::Datadog => OtelTracePropagationStyle::Datadog,
        TracePropagationStyle::TraceContext => OtelTracePropagationStyle::TraceContext,
        TracePropagationStyle::Baggage => OtelTracePropagationStyle::Baggage,
        TracePropagationStyle::B3Multi => OtelTracePropagationStyle::B3Multi,
        TracePropagationStyle::B3SingleHeader => OtelTracePropagationStyle::B3SingleHeader,
        TracePropagationStyle::None => OtelTracePropagationStyle::None,
    }
}

impl PropagationConfig for PropConfig {
    fn trace_propagation_style(&self) -> Option<&[OtelTracePropagationStyle]> {
        if self.trace_propagation_style.is_empty() {
            None
        } else {
            Some(&self.trace_propagation_style)
        }
    }

    fn trace_propagation_style_extract(&self) -> Option<&[OtelTracePropagationStyle]> {
        if self.trace_propagation_style_extract.is_empty() {
            None
        } else {
            Some(&self.trace_propagation_style_extract)
        }
    }

    fn trace_propagation_style_inject(&self) -> Option<&[OtelTracePropagationStyle]> {
        // Bottlecap does not configure injection styles separately.
        None
    }

    fn trace_propagation_extract_first(&self) -> bool {
        self.config.trace_propagation_extract_first
    }

    fn trace_propagation_behavior_extract(&self) -> TracePropagationBehaviorExtract {
        // Bottlecap does not expose DD_TRACE_PROPAGATION_BEHAVIOR_EXTRACT; upstream's
        // default (`Continue`) matches bottlecap's existing extraction behavior.
        TracePropagationBehaviorExtract::default()
    }

    fn datadog_tags_max_length(&self) -> usize {
        // Bottlecap does not expose DD_TRACE_X_DATADOG_TAGS_MAX_LENGTH; 512 is
        // upstream's default in dd-trace-rs.
        512
    }
}
