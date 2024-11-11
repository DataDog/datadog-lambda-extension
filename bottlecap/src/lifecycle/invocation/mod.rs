use base64::{engine::general_purpose, DecodeError, Engine};

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
