use std::collections::HashMap;

use datadog_trace_protobuf::pb::Span;
use rand::{rngs::OsRng, Rng, RngCore};
use serde_json::Value;
use tracing::debug;

use crate::config::AwsConfig;

use crate::lifecycle::invocation::triggers::{
    api_gateway_http_event::APIGatewayHttpEvent,
    api_gateway_rest_event::APIGatewayRestEvent,
    dynamodb_event::DynamoDbRecord,
    event_bridge_event::EventBridgeEvent,
    kinesis_event::KinesisRecord,
    sns_event::{SnsEntity, SnsRecord},
    sqs_event::SqsRecord,
    Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
};
use crate::tags::lambda::tags::{INIT_TYPE, SNAP_START_VALUE};

use super::triggers::s3_event::S3Record;

pub struct SpanInferrer {
    pub inferred_span: Option<Span>,
    pub wrapped_inferred_span: Option<Span>,
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
            wrapped_inferred_span: None,
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
        self.wrapped_inferred_span = None;
        self.is_async_span = false;
        self.carrier = None;
        self.trigger_tags = None;

        let mut trigger: Option<Box<dyn Trigger>> = None;
        let mut inferred_span = Span {
            span_id: Self::generate_span_id(),
            ..Default::default()
        };

        if APIGatewayHttpEvent::is_match(payload_value) {
            if let Some(t) = APIGatewayHttpEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                trigger = Some(Box::new(t));
            }
        } else if APIGatewayRestEvent::is_match(payload_value) {
            if let Some(t) = APIGatewayRestEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                trigger = Some(Box::new(t));
            }
        } else if SqsRecord::is_match(payload_value) {
            if let Some(t) = SqsRecord::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                // Check for SNS event wrapped in the SQS body
                if let Ok(sns_entity) = serde_json::from_str::<SnsEntity>(&t.body) {
                    debug!("Found an SNS event wrapped in the SQS body");
                    let mut wrapped_inferred_span = Span {
                        span_id: Self::generate_span_id(),
                        ..Default::default()
                    };

                    let wt = SnsRecord {
                        sns: sns_entity,
                        event_subscription_arn: None,
                    };
                    wt.enrich_span(&mut wrapped_inferred_span);
                    inferred_span.meta.extend(wt.get_tags());

                    wrapped_inferred_span.duration =
                        inferred_span.start - wrapped_inferred_span.start;

                    self.wrapped_inferred_span = Some(wrapped_inferred_span);
                } else if let Ok(event_bridge_entity) =
                    serde_json::from_str::<EventBridgeEvent>(&t.body)
                {
                    let mut wrapped_inferred_span = Span {
                        span_id: Self::generate_span_id(),
                        ..Default::default()
                    };

                    event_bridge_entity.enrich_span(&mut wrapped_inferred_span);
                    inferred_span.meta.extend(event_bridge_entity.get_tags());

                    wrapped_inferred_span.duration =
                        inferred_span.start - wrapped_inferred_span.start;

                    self.wrapped_inferred_span = Some(wrapped_inferred_span);
                };

                trigger = Some(Box::new(t));
            }
        } else if SnsRecord::is_match(payload_value) {
            if let Some(t) = SnsRecord::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                trigger = Some(Box::new(t));
            }
        } else if DynamoDbRecord::is_match(payload_value) {
            if let Some(t) = DynamoDbRecord::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                trigger = Some(Box::new(t));
            }
        } else if S3Record::is_match(payload_value) {
            if let Some(t) = S3Record::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                trigger = Some(Box::new(t));
            }
        } else if EventBridgeEvent::is_match(payload_value) {
            if let Some(t) = EventBridgeEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                trigger = Some(Box::new(t));
            }
        } else if KinesisRecord::is_match(payload_value) {
            if let Some(t) = KinesisRecord::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span);

                trigger = Some(Box::new(t));
            }
        } else {
            debug!("Unable to infer span from payload: no matching trigger found");
        }

        // Inferred a trigger
        if let Some(t) = trigger {
            let mut trigger_tags = t.get_tags();
            trigger_tags.extend([(
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                t.get_arn(&aws_config.region),
            )]);

            self.trigger_tags = Some(trigger_tags);
            self.carrier = Some(t.get_carrier());
            self.is_async_span = t.is_async();
            self.inferred_span = Some(inferred_span);
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

    // TODO: add status tag and other info from response
    // TODO: add peer.service
    pub fn complete_inferred_spans(&mut self, invocation_span: &Span) {
        if let Some(s) = &mut self.inferred_span {
            if let Some(ws) = &mut self.wrapped_inferred_span {
                // Set correct Parent ID for multiple inferred spans
                ws.parent_id = s.parent_id;
                s.parent_id = ws.span_id;

                // TODO: clean this logic
                if self.is_async_span {
                    // SNS to SQS span duration will be set
                    if ws.duration == 0 {
                        let duration = s.start - ws.start;
                        ws.duration = duration;
                    }
                } else {
                    let duration = s.start - ws.start;
                    ws.duration = duration;
                }

                // Set error
                ws.error = invocation_span.error;

                ws.trace_id = invocation_span.trace_id;
            }

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

            // Set error
            s.error = invocation_span.error;

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
