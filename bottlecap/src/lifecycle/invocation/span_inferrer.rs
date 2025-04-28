use std::collections::HashMap;

use datadog_trace_protobuf::pb::Span;
use serde_json::Value;
use tracing::debug;

use crate::traces::span_pointers::SpanPointer;
use crate::traces::{context::SpanContext, propagation::Propagator};
use crate::{
    config::aws::AwsConfig,
    lifecycle::invocation::{
        generate_span_id,
        triggers::{
            api_gateway_http_event::APIGatewayHttpEvent,
            api_gateway_rest_event::APIGatewayRestEvent,
            api_gateway_websocket_event::APIGatewayWebSocketEvent,
            dynamodb_event::DynamoDbRecord,
            event_bridge_event::EventBridgeEvent,
            kinesis_event::KinesisRecord,
            lambda_function_url_event::LambdaFunctionUrlEvent,
            msk_event::MSKEvent,
            s3_event::S3Record,
            sns_event::{SnsEntity, SnsRecord},
            sqs_event::{extract_trace_context_from_aws_trace_header, SqsRecord},
            step_function_event::StepFunctionEvent,
            Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG,
        },
    },
};

#[derive(Default)]
pub struct SpanInferrer {
    service_mapping: HashMap<String, String>,
    // Span inferred from the Lambda incoming request payload
    pub inferred_span: Option<Span>,
    // Nested span inferred from the Lambda incoming request payload
    pub wrapped_inferred_span: Option<Span>,
    // If the inferred span is async
    is_async_span: bool,
    // Carrier to extract the span context from
    carrier: Option<HashMap<String, String>>,
    // Generated Span Context from Step Functions or context taken from `AWSTraceHeader` when java->sqs->java
    generated_span_context: Option<SpanContext>,
    // Tags generated from the trigger
    trigger_tags: Option<HashMap<String, String>>,
    // Span pointers from S3 or DynamoDB streams
    pub span_pointers: Option<Vec<SpanPointer>>,
}

impl SpanInferrer {
    #[must_use]
    pub fn new(service_mapping: HashMap<String, String>) -> Self {
        Self {
            service_mapping,
            inferred_span: None,
            wrapped_inferred_span: None,
            is_async_span: false,
            carrier: None,
            generated_span_context: None,
            trigger_tags: None,
            span_pointers: None,
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

        let mut is_step_function = false;

        if APIGatewayHttpEvent::is_match(payload_value) {
            if let Some(t) = APIGatewayHttpEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                trigger = Some(Box::new(t));
            }
        } else if APIGatewayRestEvent::is_match(payload_value) {
            if let Some(t) = APIGatewayRestEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                trigger = Some(Box::new(t));
            }
        } else if APIGatewayWebSocketEvent::is_match(payload_value) {
            if let Some(t) = APIGatewayWebSocketEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                trigger = Some(Box::new(t));
            }
        } else if LambdaFunctionUrlEvent::is_match(payload_value) {
            if let Some(t) = LambdaFunctionUrlEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                trigger = Some(Box::new(t));
            }
        } else if MSKEvent::is_match(payload_value) {
            if let Some(t) = MSKEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                trigger = Some(Box::new(t));
            }
        } else if SqsRecord::is_match(payload_value) {
            if let Some(t) = SqsRecord::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                self.generated_span_context = extract_trace_context_from_aws_trace_header(
                    t.attributes.aws_trace_header.clone(),
                );

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
                    wt.enrich_span(&mut wrapped_inferred_span, &self.service_mapping);
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

                    event_bridge_entity
                        .enrich_span(&mut wrapped_inferred_span, &self.service_mapping);
                    inferred_span.meta.extend(event_bridge_entity.get_tags());

                    wrapped_inferred_span.duration =
                        inferred_span.start - wrapped_inferred_span.start;

                    self.wrapped_inferred_span = Some(wrapped_inferred_span);
                };

                trigger = Some(Box::new(t));
            }
        } else if SnsRecord::is_match(payload_value) {
            if let Some(t) = SnsRecord::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                if let Some(message) = &t.sns.message {
                    if let Ok(event_bridge_wrapper_message) =
                        serde_json::from_str::<EventBridgeEvent>(message)
                    {
                        let mut wrapped_inferred_span = Span {
                            span_id: generate_span_id(),
                            ..Default::default()
                        };

                        event_bridge_wrapper_message
                            .enrich_span(&mut wrapped_inferred_span, &self.service_mapping);
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
                t.enrich_span(&mut inferred_span, &self.service_mapping);
                self.span_pointers = t.get_span_pointers();

                trigger = Some(Box::new(t));
            }
        } else if S3Record::is_match(payload_value) {
            if let Some(t) = S3Record::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);
                self.span_pointers = t.get_span_pointers();

                trigger = Some(Box::new(t));
            }
        } else if EventBridgeEvent::is_match(payload_value) {
            if let Some(t) = EventBridgeEvent::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                trigger = Some(Box::new(t));
            }
        } else if KinesisRecord::is_match(payload_value) {
            if let Some(t) = KinesisRecord::new(payload_value.clone()) {
                t.enrich_span(&mut inferred_span, &self.service_mapping);

                trigger = Some(Box::new(t));
            }
        } else if StepFunctionEvent::is_match(payload_value) {
            if let Some(t) = StepFunctionEvent::new(payload_value.clone()) {
                self.generated_span_context = Some(t.get_span_context());
                trigger = Some(Box::new(t));
                is_step_function = true;
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
            if is_step_function && self.generated_span_context.is_some() {
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
    pub fn complete_inferred_spans(&mut self, invocation_span: &Span) {
        if let Some(s) = &mut self.inferred_span {
            s.trace_id = invocation_span.trace_id;
            s.error = invocation_span.error;
            s.meta.insert(
                String::from("peer.service"),
                invocation_span.service.clone(),
            );

            if let Some(ws) = &mut self.wrapped_inferred_span {
                ws.trace_id = invocation_span.trace_id;
                ws.error = invocation_span.error;
                ws.meta
                    .insert(String::from("peer.service"), s.service.clone());

                // The wrapper span should be the parent of the inferred span,
                // therefore the `parent_id` of the inferred span should be the
                // `span_id` of the wrapper span.
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
        }
    }

    /// Returns the span context from the inferred span if it exist.
    ///
    /// If the carrier is set, it will try to extract the span context,
    /// otherwise it will return `None`.
    ///
    pub fn get_span_context(&self, propagator: &impl Propagator) -> Option<SpanContext> {
        // Order matters here: check inferred span for trace context first, then fallback to generated span context.
        // If the order is flipped, trace propagation will be broken when AWS Xray is enabled.
        // https://github.com/DataDog/datadog-lambda-extension/pull/655
        if let Some(sc) = self.carrier.as_ref().and_then(|c| propagator.extract(c)) {
            debug!("Extracted trace context from inferred span");
            return Some(sc);
        }

        // Step Functions `SpanContext` is deterministically generated
        if self.generated_span_context.is_some() {
            debug!("Returning generated span context");
            return self.generated_span_context.clone();
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

#[cfg(test)]
mod tests {
    use crate::traces::context::{Sampling, SpanContext};
    use crate::traces::propagation::text_map_propagator::DatadogHeaderPropagator;
    use serde_json::json;
    use std::collections::HashMap;
    use std::time::Instant;

    use super::*;

    fn test_context_source(
        carrier: Option<HashMap<String, String>>,
        generated_context: Option<SpanContext>,
        expected_source: &str,
    ) {
        let mut inferrer = SpanInferrer::default();
        inferrer.carrier = carrier;
        inferrer.generated_span_context = generated_context;

        let propagator = DatadogHeaderPropagator;
        let context = inferrer.get_span_context(&propagator);

        assert!(context.is_some(), "Should return a span context");
        let context = context.unwrap();
        match expected_source {
            "inferred" => {
                assert_eq!(
                    context.trace_id, 123456789,
                    "Should have trace_id from inferred span"
                );
                assert_eq!(
                    context.span_id, 987654321,
                    "Should have span_id from inferred span"
                );
            }
            "generated" => {
                assert_eq!(
                    context.trace_id, 111111111,
                    "Should have trace_id from generated context"
                );
                assert_eq!(
                    context.span_id, 222222222,
                    "Should have span_id from generated context"
                );
            }
            "aws_trace_header" => {
                assert_eq!(
                    context.trace_id, 0x35578e774943fd9d,
                    "Should have trace_id from AWSTraceHeader"
                );
                assert_eq!(
                    context.span_id, 0x76c040bdc454a7ac,
                    "Should have span_id from AWSTraceHeader"
                );
            }
            _ => panic!("Unknown expected source: {}", expected_source),
        }
    }

    #[test]
    fn test_get_span_context_from_inferred_span() {
        let carrier = HashMap::from([
            ("x-datadog-trace-id".to_string(), "123456789".to_string()),
            ("x-datadog-parent-id".to_string(), "987654321".to_string()),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
        ]);

        let generated_context = SpanContext {
            trace_id: 111111111,
            span_id: 222222222,
            sampling: Some(Sampling {
                priority: Some(1),
                mechanism: None,
            }),
            origin: None,
            tags: HashMap::new(),
            links: Vec::new(),
        };

        // Should prefer inferred span context from carrier over generated context
        test_context_source(Some(carrier), Some(generated_context), "inferred");
    }

    #[test]
    fn test_get_span_context_fallback_to_generated() {
        let generated_context = SpanContext {
            trace_id: 111111111,
            span_id: 222222222,
            sampling: Some(Sampling {
                priority: Some(1),
                mechanism: None,
            }),
            origin: None,
            tags: HashMap::new(),
            links: Vec::new(),
        };

        // Should fallback to generated context when no carrier exists
        test_context_source(None, Some(generated_context), "generated");
    }

    #[test]
    fn test_java_sqs_aws_trace_header() {
        let mut inferrer = SpanInferrer::default();

        // Create a payload with AWSTraceHeader from Java->SQS->Java
        let payload = json!({
            "Records": [{
                "messageId": "fde33907-bdf2-4e37-bb5b-f19c4f0e5ec2",
                "receiptHandle": "test-receipt-handle",
                "body": "Hello World",
                "attributes": {
                    "ApproximateReceiveCount": "1",
                    "AWSTraceHeader": "Root=1-68029e8a-0000000035578e774943fd9d;Parent=76c040bdc454a7ac;Sampled=1",
                    "SentTimestamp": "1745002122577",
                    "SenderId": "AROAWGCM4HXUTNAMSZ533:nhulston-java-test-dev-main",
                    "ApproximateFirstReceiveTimestamp": "1745002122578"
                },
                "messageAttributes": {},
                "md5OfBody": "b10a8db164e0754105b7a99be72e3fe5",
                "eventSource": "aws:sqs",
                "eventSourceARN": "arn:aws:sqs:us-east-1:425362996713:nhulston-java",
                "awsRegion": "us-east-1"
            }]
        });

        let aws_config = AwsConfig {
            region: "us-east-1".to_string(),
            aws_access_key_id: "".to_string(),
            aws_secret_access_key: "".to_string(),
            aws_session_token: "".to_string(),
            function_name: "".to_string(),
            sandbox_init_time: Instant::now(),
            aws_container_credentials_full_uri: "".to_string(),
            aws_container_authorization_token: "".to_string(),
        };
        inferrer.infer_span(&payload, &aws_config);

        assert!(
            inferrer.generated_span_context.is_some(),
            "Should generate span context from AWSTraceHeader"
        );
        assert!(
            inferrer.carrier.is_some(),
            "Should have carrier from SQS event"
        );
        let propagator = DatadogHeaderPropagator;
        let inferred_context = inferrer
            .carrier
            .as_ref()
            .and_then(|c| propagator.extract(c));
        assert!(
            inferred_context.is_none(),
            "Carrier should not have trace context for Java->SQS->Java case"
        );

        test_context_source(
            inferrer.carrier,
            inferrer.generated_span_context,
            "aws_trace_header",
        );
    }
}
