use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    lifecycle::invocation::triggers::{
        ServiceNameResolver, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG,
    },
    traces::{
        context::{Sampling, SpanContext},
        propagation::text_map_propagator::DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY,
    },
};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LegacyStepFunctionEvent {
    #[serde(rename = "Payload")]
    pub payload: StepFunctionEvent,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct StepFunctionEvent {
    #[serde(rename = "Execution")]
    pub execution: Execution,
    #[serde(rename = "State")]
    pub state: State,
    #[serde(rename = "StateMachine")]
    pub state_machine: Option<StateMachine>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Execution {
    #[serde(rename = "Id")]
    id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct State {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "EnteredTime")]
    entered_time: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct StateMachine {
    #[serde(rename = "Id")]
    id: String,
}

impl Trigger for StepFunctionEvent {
    fn new(payload: serde_json::Value) -> Option<Self>
    where
        Self: Sized,
    {
        let p = payload.get("Payload").unwrap_or(&payload);
        match serde_json::from_value::<StepFunctionEvent>(p.clone()) {
            Ok(event) => Some(event),
            Err(e) => {
                tracing::debug!("Failed to deserialize Step Function Event: {e}");
                None
            }
        }
    }

    fn is_match(payload: &serde_json::Value) -> bool
    where
        Self: Sized,
    {
        // Check first if the payload is a Legacy Step Function event
        let p = payload.get("Payload").unwrap_or(payload);

        let execution_id = p
            .get("Execution")
            .and_then(Value::as_object)
            .and_then(|e| e.get("Id"));
        let state = p.get("State").and_then(Value::as_object);
        let name = state.and_then(|s| s.get("Name"));
        let entered_time = state.and_then(|s| s.get("EnteredTime"));

        execution_id.is_some() && name.is_some() && entered_time.is_some()
    }

    fn enrich_span(
        &self,
        _span: &mut datadog_trace_protobuf::pb::Span,
        _service_mapping: &HashMap<String, String>,
    ) {
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "states".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        if let Some(sm) = &self.state_machine {
            return sm.id.clone();
        }

        String::new()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }
}

impl StepFunctionEvent {
    #[must_use]
    pub fn get_span_context(&self) -> SpanContext {
        let (lo_tid, hi_tid) = Self::generate_trace_id(self.execution.id.clone());
        let tags = HashMap::from([(
            DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY.to_string(),
            format!("{hi_tid:x}"),
        )]);

        let parent_id = Self::generate_parent_id(
            self.execution.id.clone(),
            self.state.name.clone(),
            self.state.entered_time.clone(),
        );

        SpanContext {
            trace_id: lo_tid,
            span_id: parent_id,
            // Priority Auto Keep
            sampling: Some(Sampling {
                priority: Some(1),
                mechanism: None,
            }),
            origin: Some("states".to_string()),
            tags,
            links: vec![],
        }
    }

    /// Generates a random 64 bit ID from the formatted hash of the
    /// Step Function Execution ARN, the State Name, and the State Entered Time
    ///
    fn generate_parent_id(
        execution_id: String,
        state_name: String,
        state_entered_time: String,
    ) -> u64 {
        let unique_string = format!("{execution_id}#{state_name}#{state_entered_time}");

        let hash = Sha256::digest(unique_string.as_bytes());
        Self::get_positive_u64(&hash[0..8])
    }

    /// Generates a random 128 bit ID from the Step Function Execution ARN
    ///
    fn generate_trace_id(execution_arn: String) -> (u64, u64) {
        let hash = Sha256::digest(execution_arn.as_bytes());

        let lower_order_bits = Self::get_positive_u64(&hash[8..16]);
        let higher_order_bits = Self::get_positive_u64(&hash[0..8]);

        (lower_order_bits, higher_order_bits)
    }

    /// Converts the first 8 bytes of a byte array to a positive `u64`
    ///
    fn get_positive_u64(hash_bytes: &[u8]) -> u64 {
        let mut result: u64 = hash_bytes
            .iter()
            .take(8)
            .fold(0, |acc, &byte| (acc << 8) + u64::from(byte));

        // Ensure the highest bit is always 0
        result &= !(1u64 << 63);

        // Return 1 if result is 0
        if result == 0 {
            1
        } else {
            result
        }
    }
}

impl ServiceNameResolver for StepFunctionEvent {
    fn get_specific_identifier(&self) -> String {
        String::new()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_stepfunction"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("step_function_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = StepFunctionEvent::new(payload).expect("Failed to deserialize into Event");

        let expected = StepFunctionEvent {
            execution: Execution {
                id: String::from("arn:aws:states:us-east-1:425362996713:execution:agocsTestSF:bc9f281c-3daa-4e5a-9a60-471a3810bf44"),
            },
            state: State {
                name: String::from("agocsTest1"), 
                entered_time: String::from("2024-07-30T19:55:53.018Z"),
            },
            state_machine: Some(StateMachine {
                id: String::from("arn:aws:states:us-east-1:425362996713:stateMachine:agocsTestSF"),
            }),
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_new_legacy_event() {
        let json = read_json_file("step_function_legacy_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = StepFunctionEvent::new(payload).expect("Failed to deserialize into Event");

        let expected = StepFunctionEvent {
            execution: Execution {
                id: String::from("arn:aws:states:us-east-1:425362996713:execution:agocsTestSF:bc9f281c-3daa-4e5a-9a60-471a3810bf44"),
            },
            state: State {
                name: String::from("agocsTest1"), 
                entered_time: String::from("2024-07-30T19:55:53.018Z"),
            },
            state_machine: Some(StateMachine {
                id: String::from("arn:aws:states:us-east-1:425362996713:stateMachine:agocsTestSF"),
            }),
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("step_function_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize StepFunctionEvent");

        assert!(StepFunctionEvent::is_match(&payload));
    }

    #[test]
    fn test_is_match_legacy_event() {
        let json = read_json_file("step_function_legacy_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize StepFunctionEvent");

        assert!(StepFunctionEvent::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");
        assert!(!StepFunctionEvent::is_match(&payload));
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("step_function_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            StepFunctionEvent::new(payload).expect("Failed to deserialize StepFunctionEvent");
        let tags = event.get_tags();

        let expected = HashMap::from([(
            "function_trigger.event_source".to_string(),
            "states".to_string(),
        )]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("step_function_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            StepFunctionEvent::new(payload).expect("Failed to deserialize StepFunctionEvent");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:states:us-east-1:425362996713:stateMachine:agocsTestSF"
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("step_function_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            StepFunctionEvent::new(payload).expect("Failed to deserialize StepFunctionEvent");
        let carrier = event.get_carrier();

        let expected = HashMap::new();

        assert_eq!(carrier, expected);
    }

    #[test]
    fn get_span_context() {
        let json = read_json_file("step_function_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event =
            StepFunctionEvent::new(payload).expect("Failed to deserialize StepFunctionEvent");

        let span_context = event.get_span_context();

        let expected = SpanContext {
            trace_id: 5_744_042_798_732_701_615,
            span_id: 2_902_498_116_043_018_663,
            sampling: Some(Sampling {
                priority: Some(1),
                mechanism: None,
            }),
            origin: Some("states".to_string()),
            tags: HashMap::from([(
                DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY.to_string(),
                "1914fe7789eb32be".to_string(),
            )]),
            links: vec![],
        };

        assert_eq!(span_context, expected);
    }

    #[test]
    fn test_generate_parent_id() {
        let parent_id = StepFunctionEvent::generate_parent_id(
            String::from("arn:aws:states:sa-east-1:601427271234:express:DatadogStateMachine:acaf1a67-336a-e854-1599-2a627eb2dd8a:c8baf081-31f1-464d-971f-70cb17d01111"),
            String::from("step-one"),
            String::from("2022-12-08T21:08:19.224Z")
        );

        assert_eq!(parent_id, 4_340_734_536_022_949_921);

        let parent_id = StepFunctionEvent::generate_parent_id(
            String::from("arn:aws:states:sa-east-1:601427271234:express:DatadogStateMachine:acaf1a67-336a-e854-1599-2a627eb2dd8a:c8baf081-31f1-464d-971f-70cb17d01111"),
            String::from("step-one"),
            String::from("2022-12-08T21:08:19.224Y")
        );

        assert_eq!(parent_id, 981_693_280_319_792_699);
    }

    #[test]
    fn test_generate_trace_id() {
        let (lo_tid, hi_tid) = StepFunctionEvent::generate_trace_id(String::from(
            "arn:aws:states:sa-east-1:425362996713:stateMachine:MyStateMachine-b276uka1j",
        ));
        let hex_tid = format!("{hi_tid:x}");

        assert_eq!(lo_tid, 1_680_583_253_837_593_461);
        assert_eq!(hi_tid, 6_984_552_746_569_958_392);

        assert_eq!(hex_tid, "60ee1db79e4803f8");

        let (lo_tid, hi_tid) = StepFunctionEvent::generate_trace_id(
            String::from("arn:aws:states:us-east-1:425362996713:execution:agocsTestSF:bc9f281c-3daa-4e5a-9a60-471a3810bf44")
        );
        let hex_tid = format!("{hi_tid:x}");

        assert_eq!(lo_tid, 5_744_042_798_732_701_615);
        assert_eq!(hi_tid, 1_807_349_139_850_867_390);

        assert_eq!(hex_tid, "1914fe7789eb32be");
    }
}
