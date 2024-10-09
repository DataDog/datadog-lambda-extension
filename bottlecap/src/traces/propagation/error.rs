use thiserror::Error;

#[derive(Error, Debug, Copy, Clone)]
#[error("Cannot {} from {}, {}", operation, message, propagator_name)]
pub struct Error {
    message: &'static str,
    // which propagator this error comes from
    propagator_name: &'static str,
    // what operation was attempted
    operation: &'static str,
}

impl Error {
    /// Error when extracting a value from a carrier
    #[must_use]
    pub fn extract(message: &'static str, propagator_name: &'static str) -> Self {
        Self {
            message,
            propagator_name,
            operation: "extract",
        }
    }

    /// Error when injecting a value into a carrier
    #[allow(clippy::must_use_candidate)]
    pub fn inject(message: &'static str, propagator_name: &'static str) -> Self {
        Self {
            message,
            propagator_name,
            operation: "inject",
        }
    }
}
