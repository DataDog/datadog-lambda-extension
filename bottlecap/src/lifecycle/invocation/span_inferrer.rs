use std::collections::HashMap;
use std::sync::Arc;

use libdd_trace_protobuf::pb::Span;
use serde_json::Value;
use tracing::debug;

use crate::config::Config;
use crate::lifecycle::invocation::triggers::IdentifiedTrigger;
use crate::traces::span_pointers::SpanPointer;
use crate::traces::{context::SpanContext, propagation::Propagator};
use crate::{
    config::aws::AwsConfig,
    lifecycle::invocation::{
        generate_span_id,
        triggers::{
            FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG, Trigger,
            event_bridge_event::EventBridgeEvent,
            sns_event::{SnsEntity, SnsRecord},
            sqs_event::extract_trace_context_from_aws_trace_header,
        },
    },
};

#[derive(Default)]
pub struct SpanInferrer {
    config: Arc<Config>,
    // Span inferred from the Lambda incoming request payload
    pub inferred_span: Option<Span>,
    // Nested span inferred from the Lambda incoming request payload
    pub wrapped_inferred_span: Option<Span>,
    // If the inferred span is async
    is_async_span: bool,
    // Generated Span Context from Step Functions or context taken from `AWSTraceHeader` when java->sqs->java
    generated_span_context: Option<SpanContext>,
    // Tags generated from the trigger
    trigger_tags: Option<HashMap<String, String>>,
    // Span pointers from S3 or DynamoDB streams
    pub span_pointers: Option<Vec<SpanPointer>>,
}

impl SpanInferrer {
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            inferred_span: None,
            wrapped_inferred_span: None,
            is_async_span: false,
            generated_span_context: None,
            trigger_tags: None,
            span_pointers: None,
        }
    }

    #[must_use]
    pub fn get_trigger_type(identified_trigger: IdentifiedTrigger) -> Option<Box<dyn Trigger>> {
        match identified_trigger {
            IdentifiedTrigger::APIGatewayHttpEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::APIGatewayRestEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::APIGatewayWebSocketEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::ALBEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::LambdaFunctionUrlEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::MSKEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::SqsRecord(t) => Some(Box::new(t)),
            IdentifiedTrigger::SnsRecord(t) => Some(Box::new(t)),
            IdentifiedTrigger::DynamoDbRecord(t) => Some(Box::new(t)),
            IdentifiedTrigger::S3Record(t) => Some(Box::new(t)),
            IdentifiedTrigger::EventBridgeEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::KinesisRecord(t) => Some(Box::new(t)),
            IdentifiedTrigger::StepFunctionEvent(t) => Some(Box::new(t)),
            IdentifiedTrigger::Unknown => None,
        }
    }

    #[must_use]
    pub fn should_enrich_span(identified_trigger: &IdentifiedTrigger) -> bool {
        !matches!(
            identified_trigger,
            IdentifiedTrigger::ALBEvent(_)
                | IdentifiedTrigger::StepFunctionEvent(_)
                | IdentifiedTrigger::Unknown
        )
    }

    fn get_wrapped_inferred_span(
        identified_trigger: &IdentifiedTrigger,
        inferred_span: &mut Span,
        config: &Arc<Config>,
    ) -> Option<Span> {
        match identified_trigger {
            IdentifiedTrigger::SqsRecord(t) => {
                // Check for SNS event wrapped in the SQS body
                if let Ok(sns_entity) = serde_json::from_str::<SnsEntity>(&t.body) {
                    debug!("Found an SNS event wrapped in the SQS body");
                    let mut wrapped_inferred_span = Span {
                        span_id: generate_span_id(),
                        ..Default::default()
                    };

                    let wrapped_trigger = SnsRecord {
                        sns: sns_entity,
                        event_subscription_arn: None,
                    };
                    wrapped_trigger.enrich_span(
                        &mut wrapped_inferred_span,
                        &config.service_mapping,
                        config.trace_aws_service_representation_enabled,
                    );
                    inferred_span.meta.extend(wrapped_trigger.get_tags());

                    wrapped_inferred_span.duration =
                        inferred_span.start - wrapped_inferred_span.start;

                    return Some(wrapped_inferred_span);
                } else if let Ok(event_bridge_entity) =
                    serde_json::from_str::<EventBridgeEvent>(&t.body)
                {
                    let mut wrapped_inferred_span = Span {
                        span_id: generate_span_id(),
                        ..Default::default()
                    };

                    event_bridge_entity.enrich_span(
                        &mut wrapped_inferred_span,
                        &config.service_mapping,
                        config.trace_aws_service_representation_enabled,
                    );
                    inferred_span.meta.extend(event_bridge_entity.get_tags());

                    wrapped_inferred_span.duration =
                        inferred_span.start - wrapped_inferred_span.start;

                    return Some(wrapped_inferred_span);
                }

                None
            }
            IdentifiedTrigger::SnsRecord(t) => {
                if let Some(message) = &t.sns.message
                    && let Ok(event_bridge_wrapper_message) =
                        serde_json::from_str::<EventBridgeEvent>(message)
                {
                    let mut wrapped_inferred_span = Span {
                        span_id: generate_span_id(),
                        ..Default::default()
                    };

                    event_bridge_wrapper_message.enrich_span(
                        &mut wrapped_inferred_span,
                        &config.service_mapping,
                        config.trace_aws_service_representation_enabled,
                    );
                    inferred_span
                        .meta
                        .extend(event_bridge_wrapper_message.get_tags());

                    wrapped_inferred_span.duration =
                        inferred_span.start - wrapped_inferred_span.start;

                    return Some(wrapped_inferred_span);
                }

                None
            }
            _ => None,
        }
    }

    #[must_use]
    fn get_span_pointers(identified_trigger: &IdentifiedTrigger) -> Option<Vec<SpanPointer>> {
        match identified_trigger {
            IdentifiedTrigger::DynamoDbRecord(t) => t.get_span_pointers(),
            IdentifiedTrigger::S3Record(t) => t.get_span_pointers(),
            _ => None,
        }
    }

    #[must_use]
    fn should_skip_inferred_span(identified_trigger: &IdentifiedTrigger) -> bool {
        match identified_trigger {
            // There is no inferred span for ALB events
            IdentifiedTrigger::ALBEvent(_) => true,
            // There is no inferred span for Step Functions events
            // if the `SpanContext` is generated
            IdentifiedTrigger::StepFunctionEvent(_) => {
                extract_generated_span_context(identified_trigger).is_some()
            }
            _ => false,
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
        self.generated_span_context = None;
        self.trigger_tags = None;

        let mut inferred_span = Span {
            span_id: generate_span_id(),
            ..Default::default()
        };

        let identified_trigger = IdentifiedTrigger::from_value(payload_value);
        let should_enrich_span = Self::should_enrich_span(&identified_trigger);
        let should_skip_inferred_span = Self::should_skip_inferred_span(&identified_trigger);
        let wrapped_inferred_span =
            Self::get_wrapped_inferred_span(&identified_trigger, &mut inferred_span, &self.config);
        let span_pointers = Self::get_span_pointers(&identified_trigger);

        // Get trigger event type
        let trigger = Self::get_trigger_type(identified_trigger);

        if let Some(t) = trigger {
            if should_enrich_span {
                t.enrich_span(
                    &mut inferred_span,
                    &self.config.service_mapping,
                    self.config.trace_aws_service_representation_enabled,
                );
            }

            if let Some(dd_resource_key) = t.get_dd_resource_key(&aws_config.region) {
                inferred_span
                    .meta
                    .insert("dd_resource_key".to_string(), dd_resource_key);
            }

            self.wrapped_inferred_span = wrapped_inferred_span;
            self.span_pointers = span_pointers;

            let mut trigger_tags = t.get_tags();
            trigger_tags.insert(
                FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG.to_string(),
                t.get_arn(&aws_config.region),
            );

            self.trigger_tags = Some(trigger_tags);
            self.is_async_span = t.is_async();

            if should_skip_inferred_span {
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
            s.meta.insert("span.kind".to_string(), "server".to_string());
            let appsec_enabled = self.config.serverless_appsec_enabled;
            propagate_appsec(appsec_enabled, invocation_span, s);

            if let Some(ws) = &mut self.wrapped_inferred_span {
                ws.trace_id = invocation_span.trace_id;
                ws.error = invocation_span.error;
                ws.meta
                    .insert(String::from("peer.service"), s.service.clone());
                propagate_appsec(appsec_enabled, invocation_span, ws);

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

    /// Returns a clone of the tags associated with the inferred span
    ///
    #[must_use]
    pub fn get_trigger_tags(&self) -> Option<HashMap<String, String>> {
        self.trigger_tags.clone()
    }
}

fn propagate_appsec(
    serverless_appsec_enabled: bool,
    invocation_span: &Span,
    target_span: &mut Span,
) {
    let has_appsec = invocation_span
        .metrics
        .get("_dd.appsec.enabled")
        .copied()
        .or(if serverless_appsec_enabled {
            Some(1.0)
        } else {
            None
        });

    if let Some(enabled) = has_appsec {
        target_span
            .metrics
            .insert("_dd.appsec.enabled".to_string(), enabled);
    }

    if let Some(json) = invocation_span.meta.get("_dd.appsec.json") {
        target_span
            .meta
            .insert("_dd.appsec.json".to_string(), json.clone());
    }
}

pub fn extract_span_context(
    payload_value: &Value,
    propagator: Arc<impl Propagator>,
) -> Option<SpanContext> {
    let identified_trigger = IdentifiedTrigger::from_value(payload_value);
    let generated_span_context = extract_generated_span_context(&identified_trigger);
    let trigger = SpanInferrer::get_trigger_type(identified_trigger);

    // Order matters here: check inferred span for trace context first, then fallback to generated span context.
    // If the order is flipped, trace propagation will be broken when AWS Xray is enabled.
    // https://github.com/DataDog/datadog-lambda-extension/pull/655
    if let Some(t) = trigger {
        if let Some(sc) = propagator.extract(&t.get_carrier()) {
            debug!("Extracted trace context from inferred span");
            return Some(sc);
        }

        // Step Functions `SpanContext` is deterministically generated
        if let Some(generated_sc) = generated_span_context {
            debug!("Returning generated span context");
            return Some(generated_sc);
        }
    }

    None
}

#[must_use]
pub fn extract_generated_span_context(
    identified_trigger: &IdentifiedTrigger,
) -> Option<SpanContext> {
    match identified_trigger {
        IdentifiedTrigger::StepFunctionEvent(t) => Some(t.get_span_context()),
        IdentifiedTrigger::SqsRecord(t) => {
            extract_trace_context_from_aws_trace_header(t.attributes.aws_trace_header.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;
    use crate::traces::propagation::text_map_propagator::DatadogHeaderPropagator;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::time::Instant;

    fn test_context_source(payload: &Value, expected_source: &str) {
        let propagator = Arc::new(DatadogHeaderPropagator);
        let context = extract_span_context(payload, propagator);

        let context = context.expect("Should return a span context");
        match expected_source {
            "inferred" => {
                assert_eq!(
                    context.trace_id, 123_456_789,
                    "Should have trace_id from inferred span"
                );
                assert_eq!(
                    context.span_id, 987_654_321,
                    "Should have span_id from inferred span"
                );
            }
            "generated" => {
                // For Step Functions, the trace_id and span_id are deterministically generated
                // based on the execution ID, so we can't predict exact values
                assert!(context.trace_id > 0, "Should have a valid trace_id");
                assert!(context.span_id > 0, "Should have a valid span_id");
            }
            "aws_trace_header" => {
                assert_eq!(
                    context.trace_id, 0x3557_8e77_4943_fd9d,
                    "Should have trace_id from AWSTraceHeader"
                );
                assert_eq!(
                    context.span_id, 0x76c0_40bd_c454_a7ac,
                    "Should have span_id from AWSTraceHeader"
                );
            }
            _ => panic!("Unknown expected source: {expected_source}"),
        }
    }

    #[test]
    fn test_get_span_context_from_inferred_span() {
        // Use the exact payload from the test file
        let payload = json!({
            "version": "2.0",
            "routeKey": "GET /httpapi/get",
            "rawPath": "/httpapi/get",
            "rawQueryString": "",
            "headers": {
                "Accept": "*/*",
                "Content-Length": "0",
                "Host": "x02yirxc7a.execute-api.sa-east-1.amazonaws.com",
                "User-Agent": "curl/7.64.1",
                "X-amzn-trace-id": "Root=1-613a52fb-4c43cfc95e0241c1471bfa05",
                "X-forwarded-for": "38.122.226.210",
                "X-forwarded-port": "443",
                "X-forwarded-proto": "https",
                "X-datadog-trace-id": "123456789",
                "X-datadog-parent-id": "987654321",
                "X-datadog-sampling-priority": "1"
            },
            "requestContext": {
                "accountId": "425362996713",
                "apiId": "x02yirxc7a",
                "domainName": "x02yirxc7a.execute-api.sa-east-1.amazonaws.com",
                "domainPrefix": "x02yirxc7a",
                "http": {
                    "method": "GET",
                    "path": "/httpapi/get",
                    "protocol": "HTTP/1.1",
                    "sourceIp": "38.122.226.210",
                    "userAgent": "curl/7.64.1"
                },
                "requestId": "FaHnXjKCGjQEJ7A=",
                "routeKey": "GET /httpapi/get",
                "stage": "$default",
                "time": "09/Sep/2021:18:31:23 +0000",
                "timeEpoch": 1_631_212_283_738_i64
            },
            "isBase64Encoded": false
        });

        // Should extract trace context from inferred span
        test_context_source(&payload, "inferred");
    }

    #[test]
    fn test_get_span_context_fallback_to_generated() {
        // Use the exact payload from the test file
        let payload = json!({
            "Execution": {
                "Id": "arn:aws:states:us-east-1:425362996713:execution:agocsTestSF:bc9f281c-3daa-4e5a-9a60-471a3810bf44",
                "Input": {},
                "StartTime": "2024-07-30T19:55:52.976Z",
                "Name": "bc9f281c-3daa-4e5a-9a60-471a3810bf44",
                "RoleArn": "arn:aws:iam::425362996713:role/test-serverless-stepfunctions-dev-AgocsTestSFRole-tRkeFXScjyk4",
                "RedriveCount": 0
            },
            "StateMachine": {
                "Id": "arn:aws:states:us-east-1:425362996713:stateMachine:agocsTestSF",
                "Name": "agocsTestSF"
            },
            "State": {
                "Name": "agocsTest1",
                "EnteredTime": "2024-07-30T19:55:53.018Z",
                "RetryCount": 0
            }
        });

        // Should fallback to generated context for Step Functions
        test_context_source(&payload, "generated");
    }

    #[test]
    fn test_java_sqs_aws_trace_header() {
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

        // Should extract trace context from AWSTraceHeader
        test_context_source(&payload, "aws_trace_header");
    }

    #[test]
    fn test_span_inferrer_infer_span() {
        let config = Arc::new(Config::default());
        let mut inferrer = SpanInferrer::new(config);

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

        let aws_config = Arc::new(AwsConfig {
            region: "us-east-1".to_string(),
            aws_lwa_proxy_lambda_runtime_api: Some(String::new()),
            runtime_api: String::new(),
            function_name: String::new(),
            sandbox_init_time: Instant::now(),
            exec_wrapper: None,
            initialization_type: "on-demand".into(),
        });

        inferrer.infer_span(&payload, &aws_config);

        // Test that the inferrer processed the SQS event correctly
        assert!(
            inferrer.inferred_span.is_some(),
            "Should have inferred span from SQS event"
        );
        assert!(
            inferrer.trigger_tags.is_some(),
            "Should have trigger tags from SQS event"
        );

        // Verify the trigger tags contain the expected SQS information
        let trigger_tags = inferrer.trigger_tags.expect("Should have trigger tags");
        assert!(
            trigger_tags.contains_key("function_trigger.event_source"),
            "Should have event source in trigger tags"
        );
        assert_eq!(
            trigger_tags
                .get("function_trigger.event_source")
                .expect("Should have event source"),
            "sqs",
            "Should have SQS as event source"
        );
    }

    fn api_gateway_rest_payload() -> serde_json::Value {
        let json = read_json_file("api_gateway_rest_event.json");
        serde_json::from_str(&json).expect("Failed to deserialize API Gateway REST payload")
    }

    fn aws_config(region: &str) -> Arc<AwsConfig> {
        Arc::new(AwsConfig {
            region: region.to_string(),
            aws_lwa_proxy_lambda_runtime_api: Some(String::new()),
            runtime_api: String::new(),
            function_name: String::new(),
            sandbox_init_time: Instant::now(),
            exec_wrapper: None,
            initialization_type: "on-demand".into(),
        })
    }

    #[test]
    fn test_complete_inferred_spans_propagates_appsec_from_invocation() {
        let payload = api_gateway_rest_payload();
        let aws_config = aws_config("us-east-1");
        let mut inferrer = SpanInferrer::new(Arc::new(Config::default()));

        inferrer.infer_span(&payload, &aws_config);

        let mut invocation_span = Span {
            trace_id: 42,
            span_id: 100,
            service: "lambda-service".to_string(),
            ..Span::default()
        };
        if let Some(inferred_span) = &inferrer.inferred_span {
            invocation_span.start = inferred_span.start;
        }
        invocation_span.duration = 1;
        invocation_span
            .metrics
            .insert("_dd.appsec.enabled".to_string(), 1.0);
        invocation_span.meta.insert(
            "_dd.appsec.json".to_string(),
            r#"{"triggers":["rule"]}"#.to_string(),
        );

        inferrer.complete_inferred_spans(&invocation_span);

        let inferred_span = inferrer
            .inferred_span
            .as_ref()
            .expect("Inferred span should still be present");

        let appsec_enabled = inferred_span
            .metrics
            .get("_dd.appsec.enabled")
            .copied()
            .unwrap_or_default();
        assert!(
            (appsec_enabled - 1.0).abs() < f64::EPSILON,
            "Expected appsec enabled metric to be 1.0"
        );
        assert_eq!(
            inferred_span
                .meta
                .get("_dd.appsec.json")
                .cloned()
                .unwrap_or_default(),
            r#"{"triggers":["rule"]}"#
        );
    }

    #[test]
    fn test_complete_inferred_spans_sets_appsec_when_enabled_in_config() {
        let config = Config {
            serverless_appsec_enabled: true,
            ..Config::default()
        };
        let mut inferrer = SpanInferrer::new(Arc::new(config));

        let payload = api_gateway_rest_payload();
        let aws_config = aws_config("us-east-1");
        inferrer.infer_span(&payload, &aws_config);

        let mut invocation_span = Span {
            trace_id: 7,
            service: "lambda-service".to_string(),
            ..Span::default()
        };
        if let Some(inferred_span) = &inferrer.inferred_span {
            invocation_span.start = inferred_span.start;
        }
        invocation_span.duration = 1;

        inferrer.complete_inferred_spans(&invocation_span);

        let inferred_span = inferrer
            .inferred_span
            .as_ref()
            .expect("Inferred span should still be present");

        let appsec_enabled = inferred_span
            .metrics
            .get("_dd.appsec.enabled")
            .copied()
            .unwrap_or_default();
        assert!(
            (appsec_enabled - 1.0).abs() < f64::EPSILON,
            "Expected appsec enabled metric to be 1.0"
        );
        assert!(
            !inferred_span.meta.contains_key("_dd.appsec.json"),
            "AppSec JSON should not be added when invocation span has none"
        );
    }
}
