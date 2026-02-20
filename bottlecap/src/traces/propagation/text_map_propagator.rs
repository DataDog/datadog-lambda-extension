use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use tracing::{debug, error, warn};

use crate::traces::context::{Sampling, SpanContext};
use crate::traces::propagation::{
    Propagator,
    carrier::{Extractor, Injector},
    error::Error,
};

// Datadog Keys
pub const DATADOG_TRACE_ID_KEY: &str = "x-datadog-trace-id";
pub const DATADOG_PARENT_ID_KEY: &str = "x-datadog-parent-id";
pub const DATADOG_SPAN_ID_KEY: &str = "x-datadog-span-id";
pub const DATADOG_SAMPLING_PRIORITY_KEY: &str = "x-datadog-sampling-priority";
const DATADOG_ORIGIN_KEY: &str = "x-datadog-origin";
pub const DATADOG_TAGS_KEY: &str = "x-datadog-tags";

pub const DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY: &str = "_dd.p.tid";
const DATADOG_PROPAGATION_ERROR_KEY: &str = "_dd.propagation_error";
pub const DATADOG_LAST_PARENT_ID_KEY: &str = "_dd.parent_id";
pub const DATADOG_SAMPLING_DECISION_KEY: &str = "_dd.p.dm";

// Traceparent Keys
const TRACEPARENT_KEY: &str = "traceparent";
pub const TRACESTATE_KEY: &str = "tracestate";

pub const BAGGAGE_PREFIX: &str = "ot-baggage-";

static TRACEPARENT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^([a-f0-9]{2})-([a-f0-9]{32})-([a-f0-9]{16})-([a-f0-9]{2})(-.*)?$")
        .expect("failed creating regex")
});
static INVALID_SEGMENT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^0+$").expect("failed creating regex"));
#[allow(dead_code)]
static VALID_TAG_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^_dd\.p\.[\x21-\x2b\x2d-\x7e]+$").expect("failed creating regex")
});
#[allow(dead_code)]
static VALID_TAG_VALUE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\x20-\x2b\x2d-\x7e]*$").expect("failed creating regex"));
static INVALID_ASCII_CHARACTERS_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\x20-\x7E]+").expect("failed creating regex"));
static VALID_SAMPLING_DECISION_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-([0-9])$").expect("failed creating regex"));

#[derive(Clone, Copy)]
pub struct DatadogHeaderPropagator;

impl Propagator for DatadogHeaderPropagator {
    fn extract(&self, carrier: &dyn Extractor) -> Option<SpanContext> {
        Self::extract_context(carrier)
    }

    fn inject(&self, _context: SpanContext, _carrier: &mut dyn Injector) {
        todo!();
    }
}

impl DatadogHeaderPropagator {
    fn extract_context(carrier: &dyn Extractor) -> Option<SpanContext> {
        let trace_id = match Self::extract_trace_id(carrier) {
            Ok(trace_id) => trace_id,
            Err(e) => {
                debug!("{e}");
                return None;
            }
        };

        let parent_id = Self::extract_parent_id(carrier).unwrap_or(0);
        let sampling_priority = match Self::extract_sampling_priority(carrier) {
            Ok(sampling_priority) => sampling_priority,
            Err(e) => {
                debug!("{e}");
                return None;
            }
        };
        let origin = Self::extract_origin(carrier);
        let mut tags = Self::extract_tags(carrier);
        Self::validate_sampling_decision(&mut tags);

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
                .is_some_and(|sampling_decision| {
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

    pub fn extract_tags(carrier: &dyn Extractor) -> HashMap<String, String> {
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
        if !tags.is_empty()
            && let Some(trace_id_higher_order_bits) =
                carrier.get(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY)
            && !Self::higher_order_bits_valid(trace_id_higher_order_bits)
        {
            warn!(
                "Malformed Trace ID: {trace_id_higher_order_bits} Failed to decode trace ID from carrier."
            );
            tags.insert(
                DATADOG_PROPAGATION_ERROR_KEY.to_string(),
                format!("malformed tid {trace_id_higher_order_bits}"),
            );
            tags.remove(DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY);
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
}

struct Traceparent {
    sampling_priority: i8,
    trace_id: u128,
    span_id: u64,
}

struct Tracestate {
    sampling_priority: Option<i8>,
    origin: Option<String>,
    lower_order_trace_id: Option<String>,
}

#[derive(Clone, Copy)]
pub struct TraceContextPropagator;

impl Propagator for TraceContextPropagator {
    fn extract(&self, carrier: &dyn Extractor) -> Option<SpanContext> {
        Self::extract_context(carrier)
    }

    fn inject(&self, _context: SpanContext, _carrier: &mut dyn Injector) {
        todo!()
    }
}

impl TraceContextPropagator {
    fn extract_context(carrier: &dyn Extractor) -> Option<SpanContext> {
        let tp = carrier.get(TRACEPARENT_KEY)?.trim();

        match Self::extract_traceparent(tp) {
            Ok(traceparent) => {
                let mut tags = HashMap::new();
                tags.insert(TRACEPARENT_KEY.to_string(), tp.to_string());

                let mut origin = None;
                let mut sampling_priority = traceparent.sampling_priority;
                if let Some(ts) = carrier.get(TRACESTATE_KEY) {
                    if let Some(tracestate) = Self::extract_tracestate(ts, &mut tags) {
                        if let Some(lpid) = tracestate.lower_order_trace_id {
                            tags.insert(DATADOG_LAST_PARENT_ID_KEY.to_string(), lpid);
                        }

                        origin = tracestate.origin;

                        sampling_priority = Self::define_sampling_priority(
                            traceparent.sampling_priority,
                            tracestate.sampling_priority,
                        );
                    }
                } else {
                    debug!("No `dd` value found in tracestate");
                }

                let (trace_id_higher_order_bits, trace_id_lower_order_bits) =
                    Self::split_trace_id(traceparent.trace_id);
                tags.insert(
                    DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY.to_string(),
                    trace_id_higher_order_bits.to_string(),
                );

                Some(SpanContext {
                    trace_id: trace_id_lower_order_bits,
                    span_id: traceparent.span_id,
                    sampling: Some(Sampling {
                        priority: Some(sampling_priority),
                        mechanism: None,
                    }),
                    origin,
                    tags,
                    links: Vec::new(),
                })
            }
            Err(e) => {
                error!("Failed to extract traceparent: {e}");
                None
            }
        }
    }

    fn extract_tracestate(
        tracestate: &str,
        tags: &mut HashMap<String, String>,
    ) -> Option<Tracestate> {
        let ts_v = tracestate.split(',').map(str::trim);
        let ts = ts_v.clone().collect::<Vec<&str>>().join(",");

        if INVALID_ASCII_CHARACTERS_REGEX.is_match(&ts) {
            debug!("Received invalid tracestate header {tracestate}");
            return None;
        }

        tags.insert(TRACESTATE_KEY.to_string(), ts.clone());

        let mut dd: Option<HashMap<String, String>> = None;
        for v in ts_v.clone() {
            if let Some(stripped) = v.strip_prefix("dd=") {
                dd = Some(
                    stripped
                        .split(';')
                        .filter_map(|item| {
                            let mut parts = item.splitn(2, ':');
                            Some((parts.next()?.to_string(), parts.next()?.to_string()))
                        })
                        .collect(),
                );
            }
        }

        if let Some(dd) = dd {
            let mut tracestate = Tracestate {
                sampling_priority: None,
                origin: None,
                lower_order_trace_id: None,
            };

            if let Some(ts_sp) = dd.get("s")
                && let Ok(p_sp) = ts_sp.parse::<i8>()
            {
                tracestate.sampling_priority = Some(p_sp);
            }

            if let Some(o) = dd.get("o") {
                tracestate.origin = Some(Self::decode_tag_value(o));
            }

            if let Some(lo_tid) = dd.get("p") {
                tracestate.lower_order_trace_id = Some(lo_tid.clone());
            }

            // Convert from `t.` to `_dd.p.`
            for (k, v) in &dd {
                if let Some(stripped) = k.strip_prefix("t.") {
                    let nk = format!("_dd.p.{stripped}");
                    tags.insert(nk, Self::decode_tag_value(v));
                }
            }

            return Some(tracestate);
        }

        None
    }

    fn decode_tag_value(value: &str) -> String {
        value.replace('~', "=")
    }

    fn define_sampling_priority(
        traceparent_sampling_priority: i8,
        tracestate_sampling_priority: Option<i8>,
    ) -> i8 {
        if let Some(ts_sp) = tracestate_sampling_priority
            && ((traceparent_sampling_priority == 1 && ts_sp > 0)
                || (traceparent_sampling_priority == 0 && ts_sp < 0))
        {
            return ts_sp;
        }

        traceparent_sampling_priority
    }

    fn extract_traceparent(traceparent: &str) -> Result<Traceparent, Error> {
        let captures = TRACEPARENT_REGEX
            .captures(traceparent)
            .ok_or_else(|| Error::extract("invalid traceparent", "traceparent"))?;

        let version = &captures[1];
        let trace_id = &captures[2];
        let span_id = &captures[3];
        let flags = &captures[4];
        let tail = captures.get(5).map_or("", |m| m.as_str());

        Self::extract_version(version, tail)?;

        let trace_id = Self::extract_trace_id(trace_id)?;
        let span_id = Self::extract_span_id(span_id)?;

        let trace_flags = Self::extract_trace_flags(flags)?;
        let sampling_priority = i8::from(trace_flags & 0x1 != 0);

        Ok(Traceparent {
            sampling_priority,
            trace_id,
            span_id,
        })
    }

    fn extract_version(version: &str, tail: &str) -> Result<(), Error> {
        match version {
            "ff" => {
                return Err(Error::extract(
                    "`ff` is an invalid traceparent version",
                    "traceparent",
                ));
            }
            "00" => {
                if !tail.is_empty() {
                    return Err(Error::extract(
                        "Traceparent with version `00` should contain only 4 values delimited by `-`",
                        "traceparent",
                    ));
                }
            }
            _ => {
                warn!("Unsupported traceparent version {version}, still atempenting to parse");
            }
        }

        Ok(())
    }

    fn extract_trace_id(trace_id: &str) -> Result<u128, Error> {
        if INVALID_SEGMENT_REGEX.is_match(trace_id) {
            return Err(Error::extract(
                "`0` value for trace_id is invalid",
                "traceparent",
            ));
        }

        u128::from_str_radix(trace_id, 16)
            .map_err(|_| Error::extract("Failed to decode trace_id", "traceparent"))
    }

    #[allow(clippy::cast_possible_truncation)]
    fn split_trace_id(trace_id: u128) -> (u64, u64) {
        let trace_id_lower_order_bits = trace_id as u64;
        let trace_id_higher_order_bits = (trace_id >> 64) as u64;

        (trace_id_higher_order_bits, trace_id_lower_order_bits)
    }

    fn extract_span_id(span_id: &str) -> Result<u64, Error> {
        if INVALID_SEGMENT_REGEX.is_match(span_id) {
            return Err(Error::extract(
                "`0` value for span_id is invalid",
                "traceparent",
            ));
        }

        u64::from_str_radix(span_id, 16)
            .map_err(|_| Error::extract("Failed to decode span_id", "traceparent"))
    }

    fn extract_trace_flags(flags: &str) -> Result<u8, Error> {
        if flags.len() != 2 {
            return Err(Error::extract("Invalid trace flags length", "traceparent"));
        }

        u8::from_str_radix(flags, 16)
            .map_err(|_| Error::extract("Failed to decode trace_flags", "traceparent"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {
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

        let propagator = DatadogHeaderPropagator;

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

        let propagator = TraceContextPropagator;
        let context = propagator
            .extract(&headers)
            .expect("couldn't extract trace context");

        assert_eq!(context.trace_id, 7_277_407_061_855_694_839);
        assert_eq!(context.span_id, 67_667_974_448_284_343);
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
