use crate::traces::context::{Sampling, SpanContext};
use crate::traces::propagation::carrier::Extractor;
use crate::traces::propagation::error::Error;

// Traceparent Keys
const TRACEPARENT_KEY: &str = "traceparent";
pub(crate) const TRACESTATE_KEY: &str = "tracestate";

use crate::traces::propagation::text_map_propagator::{
    DATADOG_HIGHER_ORDER_TRACE_ID_BITS_KEY, DATADOG_LAST_PARENT_ID_KEY,
};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use tracing::{debug, error, warn};

lazy_static! {
    static ref INVALID_SEGMENT_REGEX: Regex = Regex::new(r"^0+$").expect("failed creating regex");
    static ref INVALID_ASCII_CHARACTERS_REGEX: Regex =
        Regex::new(r"[^\x20-\x7E]+").expect("failed creating regex");
    static ref TRACEPARENT_REGEX: Regex =
        Regex::new(r"(?i)^([a-f0-9]{2})-([a-f0-9]{32})-([a-f0-9]{16})-([a-f0-9]{2})(-.*)?$")
            .expect("failed creating regex");
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
fn extract(carrier: &dyn Extractor) -> Option<SpanContext> {
    let tp = carrier.get(TRACEPARENT_KEY)?.trim();

    match extract_traceparent(tp) {
        Ok(traceparent) => {
            let mut tags = HashMap::new();
            tags.insert(TRACEPARENT_KEY.to_string(), tp.to_string());

            let mut origin = None;
            let mut sampling_priority = traceparent.sampling_priority;
            if let Some(ts) = carrier.get(TRACESTATE_KEY) {
                if let Some(tracestate) = extract_tracestate(ts, &mut tags) {
                    if let Some(lpid) = tracestate.lower_order_trace_id {
                        tags.insert(DATADOG_LAST_PARENT_ID_KEY.to_string(), lpid);
                    }

                    origin = tracestate.origin;

                    sampling_priority = define_sampling_priority(
                        traceparent.sampling_priority,
                        tracestate.sampling_priority,
                    );
                }
            } else {
                debug!("No `dd` value found in tracestate");
            }

            let (trace_id_higher_order_bits, trace_id_lower_order_bits) =
                split_trace_id(traceparent.trace_id);
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

fn extract_tracestate(tracestate: &str, tags: &mut HashMap<String, String>) -> Option<Tracestate> {
    let ts_v = tracestate.split(',').map(str::trim);
    let ts = ts_v.clone().collect::<Vec<&str>>().join(",");

    if INVALID_ASCII_CHARACTERS_REGEX.is_match(&ts) {
        debug!("Received invalid tracestate header {tracestate}");
        return None;
    }

    tags.insert(TRACESTATE_KEY.to_string(), ts.to_string());

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

        if let Some(ts_sp) = dd.get("s") {
            if let Ok(p_sp) = ts_sp.parse::<i8>() {
                tracestate.sampling_priority = Some(p_sp);
            }
        }

        if let Some(o) = dd.get("o") {
            tracestate.origin = Some(decode_tag_value(o));
        }

        if let Some(lo_tid) = dd.get("p") {
            tracestate.lower_order_trace_id = Some(lo_tid.to_string());
        }

        // Convert from `t.` to `_dd.p.`
        for (k, v) in &dd {
            if let Some(stripped) = k.strip_prefix("t.") {
                let nk = format!("_dd.p.{stripped}");
                tags.insert(nk, decode_tag_value(v));
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
    if let Some(ts_sp) = tracestate_sampling_priority {
        if (traceparent_sampling_priority == 1 && ts_sp > 0)
            || (traceparent_sampling_priority == 0 && ts_sp < 0)
        {
            return ts_sp;
        }
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

    extract_version(version, tail)?;

    let trace_id = extract_trace_id(trace_id)?;
    let span_id = extract_span_id(span_id)?;

    let trace_flags = extract_trace_flags(flags)?;
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
            ))
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
