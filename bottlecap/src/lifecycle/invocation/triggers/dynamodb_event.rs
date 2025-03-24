use datadog_trace_protobuf::pb::Span;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::debug;

use crate::lifecycle::invocation::{
    processor::S_TO_NS,
    triggers::{ServiceNameResolver, Trigger, FUNCTION_TRIGGER_EVENT_SOURCE_TAG},
};
use crate::traces::span_pointers::{generate_span_pointer_hash, SpanPointer};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbEvent {
    #[serde(rename = "Records")]
    pub records: Vec<DynamoDbRecord>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbRecord {
    #[serde(rename = "dynamodb")]
    pub dynamodb: DynamoDbEntity,
    #[serde(rename = "eventID")]
    pub event_id: String,
    #[serde(rename = "eventName")]
    pub event_name: String,
    #[serde(rename = "eventVersion")]
    pub event_version: String,
    #[serde(rename = "eventSourceARN")]
    pub event_source_arn: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DynamoDbEntity {
    #[serde(rename = "ApproximateCreationDateTime")]
    pub approximate_creation_date_time: f64,
    #[serde(rename = "SizeBytes")]
    pub size_bytes: i64,
    #[serde(rename = "StreamViewType")]
    pub stream_view_type: String,
    #[serde(rename = "Keys")]
    pub keys: HashMap<String, AttributeValue>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[allow(non_snake_case)]
pub struct AttributeValue {
    #[serde(default)]
    pub S: Option<String>,
    #[serde(default)]
    pub N: Option<String>,
    #[serde(default)]
    pub B: Option<String>,
}

impl AttributeValue {
    fn get_string_value(&self) -> Option<String> {
        if let Some(s) = &self.S {
            Some(s.clone())
        } else if let Some(n) = &self.N {
            Some(n.clone())
        } else if let Some(b) = &self.B {
            Some(b.clone())
        } else {
            None
        }
    }
}

impl Trigger for DynamoDbRecord {
    fn new(payload: Value) -> Option<Self>
    where
        Self: Sized,
    {
        let records = payload.get("Records").and_then(Value::as_array);
        match records {
            Some(records) => match serde_json::from_value::<DynamoDbRecord>(records[0].clone()) {
                Ok(event) => Some(event),
                Err(e) => {
                    debug!("Failed to deserialize DynamoDB Record: {e}");
                    None
                }
            },
            None => None,
        }
    }

    fn is_match(payload: &Value) -> bool
    where
        Self: Sized,
    {
        if let Some(first_record) = payload
            .get("Records")
            .and_then(Value::as_array)
            .and_then(|r| r.first())
            .take()
        {
            first_record.get("dynamodb").is_some()
        } else {
            false
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn enrich_span(&self, span: &mut Span, service_mapping: &HashMap<String, String>) {
        debug!("Enriching an Inferred Span for a DynamoDB event");
        let table_name = self.get_specific_identifier();
        let resource = format!("{} {}", self.event_name.clone(), table_name);

        let start_time = (self.dynamodb.approximate_creation_date_time * S_TO_NS) as i64;

        let service_name = self.resolve_service_name(service_mapping, "dynamodb");

        span.name = String::from("aws.dynamodb");
        span.service = service_name.to_string();
        span.resource = resource;
        span.r#type = String::from("web");
        span.start = start_time;
        span.meta.extend(HashMap::from([
            ("operation_name".to_string(), String::from("aws.dynamodb")),
            ("event_id".to_string(), self.event_id.clone()),
            ("event_name".to_string(), self.event_name.clone()),
            ("event_version".to_string(), self.event_version.clone()),
            (
                "event_source_arn".to_string(),
                self.event_source_arn.clone(),
            ),
            (
                "size_bytes".to_string(),
                self.dynamodb.size_bytes.to_string(),
            ),
            (
                "stream_view_type".to_string(),
                self.dynamodb.stream_view_type.clone(),
            ),
            ("table_name".to_string(), table_name.to_string()),
        ]));
    }

    fn get_tags(&self) -> HashMap<String, String> {
        HashMap::from([(
            FUNCTION_TRIGGER_EVENT_SOURCE_TAG.to_string(),
            "dynamodb".to_string(),
        )])
    }

    fn get_arn(&self, _region: &str) -> String {
        self.event_source_arn.clone()
    }

    fn get_carrier(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_async(&self) -> bool {
        true
    }
}

impl ServiceNameResolver for DynamoDbRecord {
    fn get_specific_identifier(&self) -> String {
        self.event_source_arn
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string()
    }

    fn get_generic_identifier(&self) -> &'static str {
        "lambda_dynamodb"
    }
}

impl DynamoDbRecord {
    #[must_use]
    pub fn get_span_pointers(&self) -> Option<Vec<SpanPointer>> {
        if self.dynamodb.keys.is_empty() {
            return None;
        }
        let table_name = self.get_specific_identifier();

        if self.dynamodb.keys.len() == 1 {
            let (primary_key1, attr_value) = self.dynamodb.keys.iter().next().expect("No DynamoDB keys found");
            let value1 = attr_value.get_string_value()?;

            let parts = [table_name.as_str(), primary_key1.as_str(), value1.as_str(), "", ""];
            let hash = generate_span_pointer_hash(&parts);

            Some(vec![SpanPointer {
                hash,
                kind: String::from("aws.dynamodb.item"),
            }])
        } else {
            let keys: Vec<(&String, &AttributeValue)> = self.dynamodb.keys.iter().collect();

            // Sort lexicographically
            let ((primary_key1, attribute_value1), (primary_key2, attribute_value2)) = if keys[0].0 < keys[1].0 {
                (keys[0], keys[1])
            } else {
                (keys[1], keys[0])
            };

            // If unable to get string value, just return None
            let value1 = attribute_value1.get_string_value()?;
            let value2 = attribute_value2.get_string_value()?;

            let parts = [table_name.as_str(), primary_key1.as_str(), value1.as_str(), primary_key2.as_str(), value2.as_str()];
            let hash = generate_span_pointer_hash(&parts);

            Some(vec![SpanPointer {
                hash,
                kind: String::from("aws.dynamodb.item"),
            }])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::invocation::triggers::test_utils::read_json_file;

    #[test]
    fn test_new() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let result = DynamoDbRecord::new(payload).expect("Failed to deserialize into Record");

        let expected = DynamoDbRecord {
            dynamodb: DynamoDbEntity {
                approximate_creation_date_time: 1_428_537_600.0,
                size_bytes: 26,
                stream_view_type: String::from("NEW_AND_OLD_IMAGES"),
                keys: Default::default(),
            },
            event_id: String::from("c4ca4238a0b923820dcc509a6f75849b"),
            event_name: String::from("INSERT"),
            event_version: String::from("1.1"),
            event_source_arn: String::from("arn:aws:dynamodb:us-east-1:123456789012:table/ExampleTableWithStream/stream/2015-06-27T00:48:05.899"),
        };

        assert_eq!(result, expected);
    }

    #[test]
    fn test_is_match() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize DynamoDbRecord");

        assert!(DynamoDbRecord::is_match(&payload));
    }

    #[test]
    fn test_is_not_match() {
        let json = read_json_file("sqs_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize SqsRecord");
        assert!(!DynamoDbRecord::is_match(&payload));
    }

    #[test]
    fn test_enrich_span() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        let mut span = Span::default();
        let service_mapping = HashMap::new();
        event.enrich_span(&mut span, &service_mapping);
        assert_eq!(span.name, "aws.dynamodb");
        assert_eq!(span.service, "dynamodb");
        assert_eq!(span.resource, "INSERT ExampleTableWithStream");
        assert_eq!(span.r#type, "web");

        assert_eq!(
            span.meta,
            HashMap::from([
                ("operation_name".to_string(), "aws.dynamodb".to_string()),
                ("event_id".to_string(), "c4ca4238a0b923820dcc509a6f75849b".to_string()),
                ("event_name".to_string(), "INSERT".to_string()),
                ("event_version".to_string(), "1.1".to_string()),
                (
                    "event_source_arn".to_string(),
                    "arn:aws:dynamodb:us-east-1:123456789012:table/ExampleTableWithStream/stream/2015-06-27T00:48:05.899".to_string()
                ),
                ("size_bytes".to_string(), "26".to_string()),
                ("stream_view_type".to_string(), "NEW_AND_OLD_IMAGES".to_string()),
                ("table_name".to_string(), "ExampleTableWithStream".to_string()),
            ])
        );
    }

    #[test]
    fn test_get_tags() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        let tags = event.get_tags();

        let expected = HashMap::from([(
            "function_trigger.event_source".to_string(),
            "dynamodb".to_string(),
        )]);

        assert_eq!(tags, expected);
    }

    #[test]
    fn test_get_arn() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        assert_eq!(
            event.get_arn("us-east-1"),
            "arn:aws:dynamodb:us-east-1:123456789012:table/ExampleTableWithStream/stream/2015-06-27T00:48:05.899"
        );
    }

    #[test]
    fn test_get_carrier() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");
        let carrier = event.get_carrier();

        let expected = HashMap::new();

        assert_eq!(carrier, expected);
    }

    #[test]
    fn test_resolve_service_name() {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize into Value");
        let event = DynamoDbRecord::new(payload).expect("Failed to deserialize DynamoDbRecord");

        // Priority is given to the specific key
        let specific_service_mapping = HashMap::from([
            (
                "ExampleTableWithStream".to_string(),
                "specific-service".to_string(),
            ),
            ("lambda_dynamodb".to_string(), "generic-service".to_string()),
        ]);

        assert_eq!(
            event.resolve_service_name(&specific_service_mapping, "dynamodb"),
            "specific-service"
        );

        let generic_service_mapping =
            HashMap::from([("lambda_dynamodb".to_string(), "generic-service".to_string())]);
        assert_eq!(
            event.resolve_service_name(&generic_service_mapping, "dynamodb"),
            "generic-service"
        );
    }

    #[test]
    fn test_get_span_pointers_single_key() {
        let mut keys = HashMap::new();
        keys.insert(
            "id".to_string(),
            AttributeValue {
                S: Some("abc123".to_string()),
                N: None,
                B: None,
            },
        );

        let event = DynamoDbRecord {
            dynamodb: DynamoDbEntity {
                approximate_creation_date_time: 0.0,
                size_bytes: 26,
                stream_view_type: String::from("NEW_AND_OLD_IMAGES"),
                keys,
            },
            event_id: String::from("abc123"),
            event_name: String::from("INSERT"),
            event_version: String::from("1.1"),
            event_source_arn: String::from("arn:aws:dynamodb:us-east-1:123456789012:table/TestTable/stream/2015-06-27T00:48:05.899"),
        };

        let span_pointers = event.get_span_pointers().expect("Should return Some(vec)");
        assert_eq!(span_pointers.len(), 1);
        assert_eq!(span_pointers[0].kind, "aws.dynamodb.item");
        assert_eq!(span_pointers[0].hash, "69706c9e1e41a2f0cf8c0650f91cb0c2");
    }

    #[test]
    fn test_get_span_pointers_mixed_keys() {
        let mut keys = HashMap::new();
        keys.insert(
            "num_key".to_string(),
            AttributeValue {
                S: None,
                N: Some("42".to_string()),
                B: None,
            },
        );
        keys.insert(
            "bin_key".to_string(),
            AttributeValue {
                S: None,
                N: None,
                B: Some("Hello World".to_string()),
            },
        );

        let event = DynamoDbRecord {
            dynamodb: DynamoDbEntity {
                approximate_creation_date_time: 0.0,
                size_bytes: 26,
                stream_view_type: String::from("NEW_AND_OLD_IMAGES"),
                keys,
            },
            event_id: String::from("123abc"),
            event_name: String::from("INSERT"),
            event_version: String::from("1.1"),
            event_source_arn: String::from("arn:aws:dynamodb:us-east-1:123456789012:table/TestTable/stream/2015-06-27T00:48:05.899"),
        };

        let span_pointers = event.get_span_pointers().expect("Should return Some(vec)");
        assert_eq!(span_pointers.len(), 1);
        assert_eq!(span_pointers[0].kind, "aws.dynamodb.item");
        assert_eq!(span_pointers[0].hash, "2031d2d69b45adc3d5c27691924ddfcc");
    }

    #[test]
    fn test_get_span_pointers_lexicographical_ordering() {
        // Same as previous test but with keys in reverse order to test sorting
        let mut keys = HashMap::new();
        keys.insert(
            "bin_key".to_string(),
            AttributeValue {
                S: None,
                N: None,
                B: Some("Hello World".to_string()),
            },
        );
        keys.insert(
            "num_key".to_string(),
            AttributeValue {
                S: None,
                N: Some("42".to_string()),
                B: None,
            },
        );

        let event = DynamoDbRecord {
            dynamodb: DynamoDbEntity {
                approximate_creation_date_time: 0.0,
                size_bytes: 26,
                stream_view_type: String::from("NEW_AND_OLD_IMAGES"),
                keys,
            },
            event_id: String::from("123abc"),
            event_name: String::from("INSERT"),
            event_version: String::from("1.1"),
            event_source_arn: String::from("arn:aws:dynamodb:us-east-1:123456789012:table/TestTable/stream/2015-06-27T00:48:05.899"),
        };

        let span_pointers = event.get_span_pointers().expect("Should return Some(vec)");
        assert_eq!(span_pointers.len(), 1);
        assert_eq!(span_pointers[0].kind, "aws.dynamodb.item");
        assert_eq!(span_pointers[0].hash, "2031d2d69b45adc3d5c27691924ddfcc"); // same as previous test
    }
}
