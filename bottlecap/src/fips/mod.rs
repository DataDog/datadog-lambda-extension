// This module contains all of the things we do a little bit differently when we compile for FIPS
// mode. This is used in conjunction with the datadog-fips crate to ensure that when we
// compile the extension in FIPS mode, everything is built and configured correctly.

use std::env;
#[cfg(feature = "fips")]
use std::io::Error;
use std::io::Result;
use tracing::debug;

#[cfg(all(feature = "default", feature = "fips"))]
compile_error!("When building in fips mode, the default feature must be disabled");

#[must_use]
pub fn runtime_layer_would_enable_fips_mode(region: &str) -> bool {
    let is_gov_region = region.starts_with("us-gov-");

    env::var("DD_LAMBDA_FIPS_MODE")
        .map(|val| val.to_lowercase() == "true")
        .unwrap_or(is_gov_region)
}

#[cfg(feature = "fips")]
pub fn check_fips_mode_mismatch(region: &str) {
    let runtime_would_enable = runtime_layer_would_enable_fips_mode(region);
    if !runtime_would_enable {
        debug!("FIPS mode is enabled in this Extension layer but would be disabled in the runtime layer based on region and environment settings. Set DD_LAMBDA_FIPS_MODE=true or deploy the standard (non-FIPS) version of the Extension layer to ensure consistent FIPS behavior.");
    }
}

#[cfg(not(feature = "fips"))]
pub fn check_fips_mode_mismatch(region: &str) {
    let runtime_would_enable = runtime_layer_would_enable_fips_mode(region);
    if runtime_would_enable {
        debug!("FIPS mode is disabled in this Extension layer but would be enabled in the runtime layer based on region and environment settings. Deploy the FIPS version of the Extension layer or set DD_LAMBDA_FIPS_MODE=false to ensure consistent FIPS behavior.");
    }
}

#[cfg(feature = "fips")]
pub fn log_fips_status(region: &str) {
    debug!("FIPS mode is enabled");
    check_fips_mode_mismatch(region);
}

#[cfg(not(feature = "fips"))]
pub fn log_fips_status(region: &str) {
    debug!("FIPS mode is disabled");
    check_fips_mode_mismatch(region);
}

/// Sets up the client provider for TLS operations.
/// In FIPS mode, this installs the AWS-LC crypto provider.
/// In non-FIPS mode, this is a no-op.
#[cfg(feature = "fips")]
pub fn prepare_client_provider() -> Result<()> {
    rustls::crypto::default_fips_provider()
        .install_default()
        .map_err(|e| {
            Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to set up fips provider: {e:?}"),
            )
        })
}

#[cfg(not(feature = "fips"))]
// this is not unnecessary since the fips version can return an error
#[allow(clippy::unnecessary_wraps)]
pub fn prepare_client_provider() -> Result<()> {
    // No-op in non-FIPS mode
    Ok(())
}

#[cfg(not(feature = "fips"))]
#[must_use]
pub fn compute_aws_api_host(service: &String, region: &String, domain: &str) -> String {
    format!("{service}.{region}.{domain}")
}

#[cfg(feature = "fips")]
#[must_use]
pub fn compute_aws_api_host(service: &String, region: &String, domain: &str) -> String {
    format!("{service}-fips.{region}.{domain}")
}
