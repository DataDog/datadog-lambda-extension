use std::sync::Arc;

use datadog_opentelemetry::propagation::PropagationConfig;

use crate::config::{Config, TracePropagationStyle};

/// Newtype wrapper that lets us implement `PropagationConfig` for bottlecap's
/// `Config` without tripping Rust's orphan rule. Both the trait and the
/// underlying `datadog_agent_config::Config<LambdaConfig>` are foreign, so the
/// wrapper is the local type the impl can attach to. Callers that need a
/// propagator hand it an `Arc<PropConfig>` instead of an `Arc<Config>`.
#[derive(Debug, Clone)]
pub struct PropConfig(Arc<Config>);

impl PropConfig {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Arc<Self> {
        Arc::new(Self(config))
    }
}

impl PropagationConfig for PropConfig {
    fn trace_propagation_style(&self) -> Option<&[TracePropagationStyle]> {
        if self.0.trace_propagation_style.is_empty() {
            None
        } else {
            Some(&self.0.trace_propagation_style)
        }
    }

    fn trace_propagation_style_extract(&self) -> Option<&[TracePropagationStyle]> {
        if self.0.trace_propagation_style_extract.is_empty() {
            None
        } else {
            Some(&self.0.trace_propagation_style_extract)
        }
    }

    fn trace_propagation_style_inject(&self) -> Option<&[TracePropagationStyle]> {
        // Bottlecap does not configure injection styles separately.
        None
    }

    fn trace_propagation_extract_first(&self) -> bool {
        self.0.trace_propagation_extract_first
    }

    fn datadog_tags_max_length(&self) -> usize {
        // Bottlecap does not expose DD_TRACE_X_DATADOG_TAGS_MAX_LENGTH; 512 is
        // upstream's default in dd-trace-rs.
        512
    }
}
