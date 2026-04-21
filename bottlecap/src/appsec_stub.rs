//! Windows stub for the `appsec` module. `libddwaf-sys` is Unix-only, so on
//! Windows we substitute a no-op surface with the same public API.
#![allow(
    dead_code,
    unused_variables,
    clippy::unused_self,
    clippy::unused_async,
    clippy::needless_pass_by_value
)]

pub mod processor {
    use bytes::Bytes;
    use libdd_trace_protobuf::pb::Span;

    use crate::config::Config;
    use crate::lifecycle::invocation::triggers::IdentifiedTrigger;

    pub mod context {
        use std::sync::Arc;

        use libdd_trace_protobuf::pb::Span;

        use crate::config::Config;
        use crate::tags::provider::Provider;
        use crate::traces::span_pointers::SpanPointer;
        use crate::traces::trace_processor::SendingTraceProcessor;

        #[derive(Debug)]
        pub struct Context;

        impl Context {
            pub(crate) fn hold_trace(
                &mut self,
                _trace: Vec<Span>,
                _sender: SendingTraceProcessor,
                _args: HoldArguments,
            ) {
            }
        }

        pub struct HoldArguments {
            pub config: Arc<Config>,
            pub tags_provider: Arc<Provider>,
            pub body_size: usize,
            pub span_pointers: Option<Vec<SpanPointer>>,
            pub tracer_header_tags_lang: String,
            pub tracer_header_tags_lang_version: String,
            pub tracer_header_tags_lang_interpreter: String,
            pub tracer_header_tags_lang_vendor: String,
            pub tracer_header_tags_tracer_version: String,
            pub tracer_header_tags_container_id: String,
            pub tracer_header_tags_client_computed_top_level: bool,
            pub tracer_header_tags_client_computed_stats: bool,
            pub tracer_header_tags_dropped_p0_traces: usize,
            pub tracer_header_tags_dropped_p0_spans: usize,
        }
    }

    #[derive(Debug)]
    pub struct Processor;

    impl Processor {
        pub fn new(_cfg: &Config) -> Result<Self, Error> {
            Err(Error::FeatureDisabled)
        }

        pub fn service_entry_span_mut(_trace: &mut [Span]) -> Option<&mut Span> {
            None
        }

        pub fn process_span(&mut self, _span: &mut Span) -> (bool, Option<&mut context::Context>) {
            (true, None)
        }

        pub async fn process_invocation_next(&mut self, _rid: &str, _payload: &IdentifiedTrigger) {}

        pub async fn process_invocation_result(&mut self, _rid: &str, _payload: &Bytes) {}
    }

    #[derive(thiserror::Error, Debug)]
    pub enum Error {
        #[error("aap: feature is not enabled")]
        FeatureDisabled,
        #[error("aap: feature is not available on this platform")]
        Unavailable,
    }
}
