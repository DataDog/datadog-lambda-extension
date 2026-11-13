use std::env;

use crate::config::Config;

pub mod processor;

const AWS_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";
const DD_APPSEC_AWS_ACCESS_KEY_ID_TAG_ENABLED: &str = "DD_APPSEC_AWS_ACCESS_KEY_ID_TAG_ENABLED";

/// Determines whether the Serverless App & API Protection features are enabled.
#[must_use]
pub const fn is_enabled(cfg: &Config) -> bool {
    cfg.ext.serverless_appsec_enabled
}

/// Determines whether APM is only used as a transport for App & API Protection,
/// instead of being used for tracing as well.
#[must_use]
pub fn is_standalone() -> bool {
    env::var("DD_APM_TRACING_ENABLED").is_ok_and(|s| s.to_lowercase() == "true")
}

/// Determines whether AAP-monitored invocation spans carry the execution
/// environment's AWS access key ID. Read from the environment only: the key has
/// no `datadog.yaml` equivalent.
#[must_use]
pub fn is_aws_access_key_id_tag_enabled() -> bool {
    env::var(DD_APPSEC_AWS_ACCESS_KEY_ID_TAG_ENABLED)
        .is_ok_and(|s| matches!(s.to_lowercase().as_str(), "true" | "1"))
}

/// Reads the execution environment's `AWS_ACCESS_KEY_ID`. Never reads the
/// secret access key or the session token.
#[must_use]
pub fn read_aws_access_key_id() -> Option<String> {
    let value = env::var(AWS_ACCESS_KEY_ID).unwrap_or_default();
    let valid = validate_aws_access_key_id(&value);
    if valid.is_none() {
        tracing::debug!("aap: AWS_ACCESS_KEY_ID is empty or malformed, not tagging invocations");
    }
    valid
}

/// Keeps the value only if it matches `^(ASIA|AKIA)[A-Z0-9]{16}$`.
fn validate_aws_access_key_id(value: &str) -> Option<String> {
    let valid = value.len() == 20
        && (value.starts_with("ASIA") || value.starts_with("AKIA"))
        && value[4..]
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    valid.then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_aws_access_key_id() {
        for valid in ["ASIAEXAMPLE000000001", "AKIAIOSFODNN7EXAMPLE"] {
            assert_eq!(validate_aws_access_key_id(valid).as_deref(), Some(valid));
        }
        for invalid in [
            "",
            "ASIA",
            "XXXXEXAMPLE000000001",
            "asiaexample000000001",
            "ASIAEXAMPLE0000000012",
            "ASIAEXAMPLE00000000\n",
            "ASIAEXAMPLE0000000é",
        ] {
            assert_eq!(validate_aws_access_key_id(invalid), None, "{invalid:?}");
        }
    }
}
