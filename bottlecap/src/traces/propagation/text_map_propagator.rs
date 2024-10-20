use std::collections::HashMap;

use crate::traces::context::{Sampling, SpanContext};
use crate::traces::propagation::{
    carrier::Extractor,
    error::Error,
};
use lazy_static::lazy_static;
use regex::Regex;
use tracing::{debug, warn};

// Datadog Keys
const DATADOG_TRACE_ID_KEY: &str = "x-datadog-trace-id";
const DATADOG_PARENT_ID_KEY: &str = "x-datadog-parent-id";
const DATADOG_SAMPLING_PRIORITY_KEY: &str = "x-datadog-sampling-priority";
const DATADOG_ORIGIN_KEY: &str = "x-datadog-origin";
const DATADOG_TAGS_KEY: &str = "x-datadog-tags";

pub const DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY: &str = "_dd.p.tid";
const DATADOG_PROPAGATION_ERROR_KEY: &str = "_dd.propagation_error";
pub const DATADOG_LAST_PARENT_ID_KEY: &str = "_dd.parent_id";
const DATADOG_SAMPLING_DECISION_KEY: &str = "_dd.p.dm";

pub const BAGGAGE_PREFIX: &str = "ot-baggage-";

lazy_static! {
    static ref INVALID_SEGMENT_REGEX: Regex = Regex::new(r"^0+$").expect("failed creating regex");
    static ref VALID_TAG_KEY_REGEX: Regex =
        Regex::new(r"^_dd\.p\.[\x21-\x2b\x2d-\x7e]+$").expect("failed creating regex");
    static ref VALID_TAG_VALUE_REGEX: Regex =
        Regex::new(r"^[\x20-\x2b\x2d-\x7e]*$").expect("failed creating regex");
    static ref INVALID_ASCII_CHARACTERS_REGEX: Regex =
        Regex::new(r"[^\x20-\x7E]+").expect("failed creating regex");
    static ref VALID_SAMPLING_DECISION_REGEX: Regex =
        Regex::new(r"^-([0-9])$").expect("failed creating regex");
}
pub(crate) fn extract_datadog(carrier: &dyn Extractor) -> Option<SpanContext> {
    let trace_id = match extract_trace_id(carrier) {
        Ok(trace_id) => trace_id,
        Err(e) => {
            debug!("{e}");
            return None;
        }
    };

    let parent_id = extract_parent_id(carrier).unwrap_or(0);
    let sampling_priority = match extract_sampling_priority(carrier) {
        Ok(sampling_priority) => sampling_priority,
        Err(e) => {
            debug!("{e}");
            return None;
        }
    };
    let origin = extract_origin(carrier);
    let mut tags = extract_tags(carrier);
    validate_sampling_decision(&mut tags);

    Some(SpanContext {
        trace_id,
        span_id: parent_id,
        sampling: Some(Sampling {
            priority: Some(sampling_priority),
            mechanism: None,
        }),
        origin,
        tags,
        links: Vec::new(),
    })
}

fn validate_sampling_decision(tags: &mut HashMap<String, String>) {
    let should_remove =
        tags.get(DATADOG_SAMPLING_DECISION_KEY)
            .map_or(false, |sampling_decision| {
                let is_invalid = !VALID_SAMPLING_DECISION_REGEX.is_match(sampling_decision);
                if is_invalid {
                    warn!("Failed to decode `_dd.p.dm`: {}", sampling_decision);
                }
                is_invalid
            });

    if should_remove {
        tags.remove(DATADOG_SAMPLING_DECISION_KEY);
        tags.insert(
            DATADOG_PROPAGATION_ERROR_KEY.to_string(),
            "decoding_error".to_string(),
        );
    }

    // todo: appsec standalone
}

fn extract_trace_id(carrier: &dyn Extractor) -> Result<u64, Error> {
    let trace_id = carrier
        .get(DATADOG_TRACE_ID_KEY)
        .ok_or(Error::extract("`trace_id` not found", "datadog"))?;

    if INVALID_SEGMENT_REGEX.is_match(trace_id) {
        return Err(Error::extract("Invalid `trace_id` found", "datadog"));
    }

    trace_id
        .parse::<u64>()
        .map_err(|_| Error::extract("Failed to decode `trace_id`", "datadog"))
}

fn extract_parent_id(carrier: &dyn Extractor) -> Option<u64> {
    let parent_id = carrier.get(DATADOG_PARENT_ID_KEY)?;

    parent_id.parse::<u64>().ok()
}

fn extract_sampling_priority(carrier: &dyn Extractor) -> Result<i8, Error> {
    // todo: enum? Default is USER_KEEP=2
    let sampling_priority = carrier.get(DATADOG_SAMPLING_PRIORITY_KEY).unwrap_or("2");

    sampling_priority
        .parse::<i8>()
        .map_err(|_| Error::extract("Failed to decode `sampling_priority`", "datadog"))
}

fn extract_origin(carrier: &dyn Extractor) -> Option<String> {
    let origin = carrier.get(DATADOG_ORIGIN_KEY)?;
    Some(origin.to_string())
}

fn extract_tags(carrier: &dyn Extractor) -> HashMap<String, String> {
    let carrier_tags = carrier.get(DATADOG_TAGS_KEY).unwrap_or_default();
    let mut tags: HashMap<String, String> = HashMap::new();

    // todo:
    // - trace propagation disabled
    // - trace propagation max lenght

    let pairs = carrier_tags.split(',');
    for pair in pairs {
        if let Some((k, v)) = pair.split_once('=') {
            // todo: reject key on tags extract reject
            if k.starts_with("_dd.p.") {
                tags.insert(k.to_string(), v.to_string());
            }
        }
    }

    // Handle 128bit trace ID
    if !tags.is_empty() {
        if let Some(trace_id_higher_order_bits) =
            carrier.get(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY)
        {
            if !higher_order_bits_valid(trace_id_higher_order_bits) {
                warn!("Malformed Trace ID: {trace_id_higher_order_bits} Failed to decode trace ID from carrier.");
                tags.insert(
                    DATADOG_PROPAGATION_ERROR_KEY.to_string(),
                    format!("malformed tid {trace_id_higher_order_bits}"),
                );
                tags.remove(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY);
            }
        }
    }

    if !tags.contains_key(DATADOG_SAMPLING_DECISION_KEY) {
        tags.insert(DATADOG_SAMPLING_DECISION_KEY.to_string(), "-3".to_string());
    }

    tags
}

fn higher_order_bits_valid(trace_id_higher_order_bits: &str) -> bool {
    if trace_id_higher_order_bits.len() != 16 {
        return false;
    }

    match u64::from_str_radix(trace_id_higher_order_bits, 16) {
        Ok(_) => {}
        Err(_) => return false,
    }

    true
}

#[cfg(test)]
mod test {
    use crate::config::trace_propagation_style::TracePropagationStyle;
    use super::*;

    #[test]
    fn test_extract_datadog_propagator() {
        let headers = HashMap::from([
            ("x-datadog-trace-id".to_string(), "1234".to_string()),
            ("x-datadog-parent-id".to_string(), "5678".to_string()),
            ("x-datadog-sampling-priority".to_string(), "1".to_string()),
            ("x-datadog-origin".to_string(), "synthetics".to_string()),
            (
                "x-datadog-tags".to_string(),
                "_dd.p.test=value,_dd.p.tid=4321,any=tag".to_string(),
            ),
        ]);

        let propagator = TracePropagationStyle::Datadog;

        let context = propagator
            .extract(&headers)
            .expect("couldn't extract trace context");

        assert_eq!(context.trace_id, 1234);
        assert_eq!(context.span_id, 5678);
        assert_eq!(context.sampling.unwrap().priority, Some(1));
        assert_eq!(context.origin, Some("synthetics".to_string()));
        println!("{:?}", context.tags);
        assert_eq!(context.tags.get("_dd.p.test").unwrap(), "value");
        assert_eq!(context.tags.get("_dd.p.tid").unwrap(), "4321");
        assert_eq!(context.tags.get("_dd.p.dm").unwrap(), "-3");
    }

    #[test]
    fn test_extract_traceparent_propagator() {
        let headers = HashMap::from([
            (
                "traceparent".to_string(),
                "00-80f198ee56343ba864fe8b2a57d3eff7-00f067aa0ba902b7-01".to_string(),
            ),
            (
                "tracestate".to_string(),
                "dd=p:00f067aa0ba902b7;s:2;o:rum".to_string(),
            ),
        ]);

        let propagator = TracePropagationStyle::TraceContext;
        let context = propagator
            .extract(&headers)
            .expect("couldn't extract trace context");

        assert_eq!(context.trace_id, 7277407061855694839);
        assert_eq!(context.span_id, 67667974448284343);
        assert_eq!(context.sampling.unwrap().priority, Some(2));
        assert_eq!(context.origin, Some("rum".to_string()));
        assert_eq!(
            context.tags.get("traceparent").unwrap(),
            "00-80f198ee56343ba864fe8b2a57d3eff7-00f067aa0ba902b7-01"
        );
        assert_eq!(
            context.tags.get("tracestate").unwrap(),
            "dd=p:00f067aa0ba902b7;s:2;o:rum"
        );
        assert_eq!(
            context.tags.get("_dd.p.tid").unwrap(),
            "9291375655657946024"
        );
        assert_eq!(
            context.tags.get("_dd.parent_id").unwrap(),
            "00f067aa0ba902b7"
        );
    }
}
