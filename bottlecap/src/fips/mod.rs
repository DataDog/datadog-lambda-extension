// This module contains all of the things we do a little bit differently when we compile for FIPS
// mode. This is used in conjunction with the datadog-serverless-fips crate to ensure that when we
// compile the extension in FIPS mode, everything is built and configured correctly.

#[cfg(feature = "fips")]
use std::io::Error;
use std::io::Result;
use tracing::debug;

#[cfg(all(feature = "default", feature = "fips"))]
compile_error!("When building in fips mode, the default feature must be disabled");

#[cfg(feature = "fips")]
pub fn log_fips_status() {
    debug!("FIPS mode is enabled");
}

#[cfg(not(feature = "fips"))]
pub fn log_fips_status() {
    debug!("FIPS mode is disabled");
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
