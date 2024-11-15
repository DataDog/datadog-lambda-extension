use std::collections::HashMap;

use datadog_trace_protobuf::pb::Span;
use serde_json::Value;
use tracing::debug;

use crate::config::AwsConfig;

use crate::lifecycle::invocation::{
    generate_span_id,
    triggers::{
        api_gateway_http_event::APIGatewayHttpEvent,
        api_gateway_rest_event::APIGatewayRestEvent,
        dynamodb_event::DynamoDbRecord,
        event_bridge_event::EventBridgeEvent,
        kinesis_event::KinesisRecord,
        lambda_function_url_event::LambdaFunctionUrlEvent,
        s3_event::S3Record,
        sns_event::{SnsEntity, SnsRecord},
        sqs_event::SqsRecord,
        step_function_event::StepFunctionEvent,
        Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
    },
};
use crate::traces::{context::SpanContext, propagation::Propagator};

pub struct SpanInferrer {
    // Span inferred from the Lambda incoming request payload
    pub inferred_span: Option<Span>,
    // Nested span inferred from the Lambda incoming request payload
    pub wrapped_inferred_span: Option<Span>,
    // If the inferred span is async
    is_async_span: bool,
    // Carrier to extract the span context from
    carrier: Option<HashMap<String, String>>,
    // Generated Span Context from Step Functions
    generated_span_context: Option<SpanContext>,
    // Tags generated from the trigger
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
            generated_span_context: None,
            trigger_tags: None,
        }
    }

    /// Given a byte payload, try to deserialize it into a `serde_json::Value`
    /// and try matching it to a `Trigger` implementation, which will create
    /// an inferred span and set it to `self.inferred_span`
    ///
    #[allow(clippy::too_many_lines)]
    pub fn infer_span(&mut self, payload_value: &Value, aws_config: &AwsConfig) {
        self.inferred_span = None;
        self.wrapped_inferred_span = None;
        self.is_async_span = false;
        self.carrier = None;
        self.generated_span_context = None;
        self.trigger_tags = None;

        let mut trigger: Option<Box<dyn Trigger>> = None;
        let mut inferred_span = Span {
            span_id: generate_span_id(),
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
        } else if LambdaFunctionUrlEvent::is_match(payload_value) {
            if let Some(t) = LambdaFunctionUrlEvent::new(payload_value.clone()) {
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
                        span_id: generate_span_id(),
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
                        span_id: generate_span_id(),
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

                if let Some(message) = &t.sns.message {
                    if let Ok(event_bridge_wrapper_message) =
                        serde_json::from_str::<EventBridgeEvent>(message)
                    {
                        let mut wrapped_inferred_span = Span {
                            span_id: generate_span_id(),
                            ..Default::default()
                        };

                        event_bridge_wrapper_message.enrich_span(&mut wrapped_inferred_span);
                        inferred_span
                            .meta
                            .extend(event_bridge_wrapper_message.get_tags());

                        wrapped_inferred_span.duration =
                            inferred_span.start - wrapped_inferred_span.start;

                        self.wrapped_inferred_span = Some(wrapped_inferred_span);
                    }
                }

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
        } else if StepFunctionEvent::is_match(payload_value) {
            if let Some(t) = StepFunctionEvent::new(payload_value.clone()) {
                self.generated_span_context = Some(t.get_span_context());
                trigger = Some(Box::new(t));
            }
        } else {
            debug!("Unable to infer span from payload: no matching trigger found");
        }

        // Inferred a trigger
        if let Some(t) = trigger {
            let mut trigger_tags = t.get_tags();
            trigger_tags.insert(
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                t.get_arn(&aws_config.region),
            );

            self.trigger_tags = Some(trigger_tags);
            self.carrier = Some(t.get_carrier());
            self.is_async_span = t.is_async();

            // For Step Functions, there is no inferred span
            if self.generated_span_context.is_some() {
                self.inferred_span = None;
            } else {
                self.inferred_span = Some(inferred_span);
            }
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

    /// Returns a clone of the carrier associated with the inferred span
    ///
    /// If the carrier is set, it will try to extract the span context,
    /// otherwise it will
    ///
    pub fn get_span_context(&self, propagator: &impl Propagator) -> Option<SpanContext> {
        // Step Functions `SpanContext` is deterministically generated
        if let Some(sc) = &self.generated_span_context {
            return Some(sc.clone());
        }

        if let Some(sc) = self.carrier.as_ref().and_then(|c| propagator.extract(c)) {
            debug!("Extracted trace context from inferred span");
            return Some(sc);
        }

        None
    }

    /// Returns a clone of the tags associated with the inferred span
    ///
    #[must_use]
    pub fn get_trigger_tags(&self) -> Option<HashMap<String, String>> {
        self.trigger_tags.clone()
    }
}
