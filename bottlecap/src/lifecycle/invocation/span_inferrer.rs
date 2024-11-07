use std::collections::HashMap;

use datadog_trace_protobuf::pb::Span;
use rand::{rngs::OsRng, Rng, RngCore};
use serde_json::Value;
use tracing::debug;

use crate::config::AwsConfig;

use crate::lifecycle::invocation::triggers::{
    api_gateway_http_event::APIGatewayHttpEvent, api_gateway_rest_event::APIGatewayRestEvent,
    event_bridge_event::EventBridgeEvent,sqs_event::SqsRecord, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
};
use crate::tags::lambda::tags::{INIT_TYPE, SNAP_START_VALUE};

pub struct SpanInferrer {
    pub inferred_span: Option<Span>,
    is_async_span: bool,
    carrier: Option<HashMap<String, String>>,
    trigger_tags: Option<HashMap<String, String>>,
}

impl Default for SpanInferrer {
    fn default() -> Self {
        Self::new()
    }
}

impl SpanInferrer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inferred_span: None,
            is_async_span: false,
            carrier: None,
            trigger_tags: None,
        }
    }

    /// Given a byte payload, try to deserialize it into a `serde_json::Value`
    /// and try matching it to a `Trigger` implementation, which will create
    /// an inferred span and set it to `self.inferred_span`
    ///
    pub fn infer_span(&mut self, payload_value: &Value, aws_config: &AwsConfig) {
        self.inferred_span = None;
        self.is_async_span = false;
        self.carrier = None;
        self.trigger_tags = None;

        if APIGatewayHttpEvent::is_match(payload_value) {
            if let Some(t) = APIGatewayHttpEvent::new(payload_value.clone()) {
                let mut span = Span {
                    span_id: Self::generate_span_id(),
                    ..Default::default()
                };

                t.enrich_span(&mut span);
                let mut tt = t.get_tags();
                tt.extend([(
                    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                    t.get_arn(&aws_config.region),
                )]);

                self.carrier = Some(t.get_carrier());
                self.trigger_tags = Some(tt);
                self.is_async_span = t.is_async();
                self.inferred_span = Some(span);
            }
        } else if APIGatewayRestEvent::is_match(payload_value) {
            if let Some(t) = APIGatewayRestEvent::new(payload_value.clone()) {
                let mut span = Span {
                    span_id: Self::generate_span_id(),
                    ..Default::default()
                };

                t.enrich_span(&mut span);
                let mut tt = t.get_tags();
                tt.extend([(
                    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                    t.get_arn(&aws_config.region),
                )]);

                self.carrier = Some(t.get_carrier());
                self.trigger_tags = Some(tt);
                self.is_async_span = t.is_async();
                self.inferred_span = Some(span);
            }
        } else if SqsRecord::is_match(payload_value) {
            if let Some(t) = SqsRecord::new(payload_value.clone()) {
                let mut span = Span {
                    span_id: Self::generate_span_id(),
                    ..Default::default()
                };

                t.enrich_span(&mut span);
                let mut tt = t.get_tags();
                tt.extend([(
                    FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                    t.get_arn(&aws_config.region),
                )]);

                self.carrier = Some(t.get_carrier());
                self.trigger_tags = Some(tt);
                self.is_async_span = t.is_async();
                self.inferred_span = Some(span);
            }
        } else if EventBridgeEvent::is_match(payload_value) {
            if let Some(t) = EventBridgeEvent::new(payload_value.clone()) {
                let mut span = Span {
                    span_id: Self::generate_span_id(),
                    ..Default::default()
                };

                t.enrich_span(&mut span);
                span.meta.extend([
                    (
                        FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
                        "eventbridge".to_string(),
                    ),
                    (
                        FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                        t.get_arn(&aws_config.region),
                    ),
                ]);

                self.carrier = Some(t.get_carrier());
                self.trigger_tags = Some(t.get_tags());
                self.is_async_span = t.is_async();
                self.inferred_span = Some(span);
            }
        } else {
            debug!("Unable to infer span from payload: no matching trigger found");
        }
    }

    /// If a `self.inferred_span` exist, set the `parent_id` to
    /// the span.
    ///
    pub fn set_parent_id(&mut self, parent_id: u64) {
        if let Some(s) = &mut self.inferred_span {
            s.parent_id = parent_id;
        }
    }

    pub fn extend_meta(&mut self, iter: HashMap<String, String>) {
        if let Some(s) = &mut self.inferred_span {
            s.meta.extend(iter);
        }
    }

    pub fn set_status_code(&mut self, status_code: String) {
        if let Some(s) = &mut self.inferred_span {
            s.meta.insert("http.status_code".to_string(), status_code);
        }
    }

    // TODO add status tag and other info from response
    pub fn complete_inferred_span(&mut self, invocation_span: &Span) {
        if let Some(s) = &mut self.inferred_span {
            if self.is_async_span {
                // SNS to SQS span duration will be set
                if s.duration == 0 {
                    let duration = invocation_span.start - s.start;
                    s.duration = duration;
                }
            } else {
                let duration = (invocation_span.start + invocation_span.duration) - s.start;
                s.duration = duration;
            }

            s.trace_id = invocation_span.trace_id;
        }
    }

    fn generate_span_id() -> u64 {
        if std::env::var(INIT_TYPE).map_or(false, |it| it == SNAP_START_VALUE) {
            return OsRng.next_u64();
        }

        let mut rng = rand::thread_rng();
        rng.gen()
    }

    /// Returns a clone of the carrier associated with the inferred span
    ///
    #[must_use]
    pub fn get_carrier(&self) -> Option<HashMap<String, String>> {
        self.carrier.clone()
    }

    /// Returns a clone of the tags associated with the inferred span
    ///
    #[must_use]
    pub fn get_trigger_tags(&self) -> Option<HashMap<String, String>> {
        self.trigger_tags.clone()
    }
}
