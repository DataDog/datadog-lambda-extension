use std::collections::HashMap;
use std::sync::LazyLock;

use libdd_trace_protobuf::pb::Span;
use regex::Regex;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub mod body;
mod serde_utils;

pub mod alb_event;
pub mod api_gateway_http_event;
pub mod api_gateway_rest_event;
pub mod api_gateway_websocket_event;
pub mod dynamodb_event;
pub mod event_bridge_event;
pub mod kinesis_event;
pub mod lambda_function_url_event;
pub mod msk_event;
pub mod s3_event;
pub mod sns_event;
pub mod sqs_event;
pub mod step_function_event;

pub const DATADOG_CARRIER_KEY: &str = "_datadog";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_TAG: &str = "function_trigger.event_source";
pub const FUNCTION_TRIGGER_EVENT_SOURCE_ARN_TAG: &str = "function_trigger.event_source_arn";
static ULID_UUID_GUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (
            [0-9a-fA-F]{8}-          # UUID/GUID segment 1
            [0-9a-fA-F]{4}-          # segment 2
            [0-9a-fA-F]{4}-          # segment 3
            [0-9a-fA-F]{4}-          # segment 4
            [0-9a-fA-F]{12}          # segment 5
        )
        |
        (
            [0123456789ABCDEFGHJKMNPQRSTVWXYZ]{26}  # ULID
        )
    ",
    )
    .expect("failed to create regex")
});

/// Resolves the service name for a given trigger depending on
/// service mapping configuration.
pub trait ServiceNameResolver {
    /// Get the specific service name for this trigger type, it will
    /// be used as a key to resolve the service name
    fn get_specific_identifier(&self) -> String;

    /// Get the generic service mapping key for the trigger
    fn get_generic_identifier(&self) -> &'static str;
}

#[must_use]
pub fn parameterize_api_resource(resource: String) -> String {
    // curly braces are used for APIGW parameters feature
    if resource.contains('{') && resource.contains('}') {
        return resource;
    }

    let parts: Vec<&str> = resource.split('/').collect();
    let mut result = Vec::new();

    // First element is empty string due to leading slash
    result.push(String::new());

    // Process each path segment
    for (i, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            continue;
        }

        // Check if this part looks like an identifier
        // Number, ULID, GUID, or UUID
        if part.chars().all(|c| c.is_ascii_digit()) || ULID_UUID_GUID.is_match(part) {
            // Determine the parameter name based on the previous segment
            let param_name = if i > 1 && !parts[i - 1].is_empty() {
                let singular = parts[i - 1].trim_end_matches('s');
                if singular == "id" {
                    singular.into()
                } else {
                    format!("{singular}_id")
                }
            } else {
                "id".to_string()
            };

            // Format the parameter with braces and store it in the result
            result.push(format!("{{{param_name}}}"));
        } else {
            result.push((*part).to_string());
        }
    }
    result.join("/")
}

#[must_use]
pub fn get_default_service_name(
    instance_name: &str,
    fallback: &str,
    aws_service_representation_enabled: bool,
) -> String {
    if !aws_service_representation_enabled {
        return fallback.to_string();
    }

    instance_name.to_string()
}

pub trait Trigger: ServiceNameResolver {
    fn new(payload: Value) -> Option<Self>
    where
        Self: Sized;
    fn is_match(payload: &Value) -> bool
    where
        Self: Sized;
    fn enrich_span(
        &self,
        span: &mut Span,
        service_mapping: &HashMap<String, String>,
        aws_service_representation_enabled: bool,
    );
    fn get_tags(&self) -> HashMap<String, String>;
    fn get_arn(&self, region: &str) -> String;
    fn get_carrier(&self) -> HashMap<String, String>;
    fn is_async(&self) -> bool;

    fn get_dd_resource_key(&self, _region: &str) -> Option<String> {
        None
    }

    /// Default implementation for service name resolution
    fn resolve_service_name(
        &self,
        service_mapping: &HashMap<String, String>,
        instance_name: &str,
        fallback: &str,
        aws_service_representation_enabled: bool,
    ) -> String {
        service_mapping
            .get(&self.get_specific_identifier())
            .or_else(|| service_mapping.get(self.get_generic_identifier()))
            .cloned()
            .unwrap_or_else(|| {
                get_default_service_name(
                    instance_name,
                    fallback,
                    aws_service_representation_enabled,
                )
            })
    }
}

/// A macro do define an enum for all the know trigger types.
///
/// It generates an enum with one variant for each named type.
/// The variants are specified as `<data-type> => <variant-name>`.
///
/// It also creates `from_value` and `from_slice` methods that use the
/// [`Trigger::is_match`] and [`Trigger::new`] methods to try and parse a
/// payload; cases are matched in the order they are declared.
macro_rules! identified_triggers {
    (
        $vis:vis enum $name:ident {
            $($type:ty => $case:ident),+,
            else => $default:ident,
        }
    ) => {
        #[derive(Debug, Default)]
        #[must_use]
        #[non_exhaustive]
        $vis enum $name {
            $($case($type),)+
            #[default]
            $default,
        }
        impl $name {
            $vis fn from_value(payload: &Value) -> Self {
                $(
                if <$type>::is_match(payload) {
                    return <$type>::new(payload.clone()).map_or(Self::$default, Self::$case);
                }
                )+
                Self::$default
            }

            $vis fn from_slice(payload: &[u8]) -> serde_json::Result<Self> {
                let value = serde_json::from_slice(payload)?;
                Ok(Self::from_value(&value))
            }

            #[doc = concat!("Returns `true` if this trigger is [`Self::", stringify!($default), "`].")]
            #[must_use]
            $vis const fn is_unknown(&self) -> bool {
                matches!(self, Self::$default)
            }
        }
    };
}

identified_triggers!(
    pub enum IdentifiedTrigger {
        api_gateway_http_event::APIGatewayHttpEvent => APIGatewayHttpEvent,
        api_gateway_rest_event::APIGatewayRestEvent => APIGatewayRestEvent,
        api_gateway_websocket_event::APIGatewayWebSocketEvent => APIGatewayWebSocketEvent,
        alb_event::ALBEvent => ALBEvent,
        lambda_function_url_event::LambdaFunctionUrlEvent => LambdaFunctionUrlEvent,
        msk_event::MSKEvent => MSKEvent,
        sqs_event::SqsRecord => SqsRecord,
        sns_event::SnsRecord => SnsRecord,
        dynamodb_event::DynamoDbRecord => DynamoDbRecord,
        s3_event::S3Record => S3Record,
        event_bridge_event::EventBridgeEvent => EventBridgeEvent,
        kinesis_event::KinesisRecord => KinesisRecord,
        step_function_event::StepFunctionEvent => StepFunctionEvent,
        else => Unknown,
    }
);

/// Serialize a `HashMap` with lowercase keys
///
pub fn lowercase_key<'de, D, V>(deserializer: D) -> Result<HashMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    let map = HashMap::<String, V>::deserialize(deserializer)?;
    Ok(map
        .into_iter()
        .map(|(key, value)| (key.to_lowercase(), value))
        .collect())
}

#[cfg(test)]
pub mod test_utils {
    use std::fs;
    use std::path::PathBuf;

    #[must_use]
    pub fn read_json_file(file_name: &str) -> String {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/payloads");
        path.push(file_name);
        fs::read_to_string(path).expect("Failed to read file")
    }
}
