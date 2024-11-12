use base64::{engine::general_purpose, DecodeError, Engine};
use datadog_trace_protobuf::pb::Span;
use rand::{rngs::OsRng, Rng, RngCore};

use crate::tags::lambda::tags::{INIT_TYPE, SNAP_START_VALUE};

pub mod context;
pub mod processor;
pub mod span_inferrer;
pub mod triggers;

pub fn base64_to_string(base64_string: &str) -> Result<String, DecodeError> {
    match general_purpose::STANDARD.decode(base64_string) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).to_string()),
        Err(e) => Err(e),
    }
}

fn create_empty_span(name: String, resource: String, service: String) -> Span {
    Span {
        name,
        resource,
        service,
        r#type: String::from("serverless"),
        ..Default::default()
    }
}

fn generate_span_id() -> u64 {
    if std::env::var(INIT_TYPE).map_or(false, |it| it == SNAP_START_VALUE) {
        return OsRng.next_u64();
    }

    let mut rng = rand::thread_rng();
    rng.gen()
}
