use reqwest::ClientBuilder;
use std::error::Error;
#[cfg(feature = "fips")]
use tracing::debug;

// TODO: once we confirm that this does what we think it does we'll move it to a separate crate.
// for now going to copy the this code to bottlecap and make sure that all the clients we build do
// in fact do the fips thing right.

/// Creates a reqwest client builder with TLS configuration.
/// When the "fips" feature is enabled, it uses a FIPS-compliant TLS configuration.
/// Otherwise, it uses reqwest's default rustls TLS implementation.
#[cfg(not(feature = "fips"))]
pub fn create_reqwest_client_builder() -> Result<ClientBuilder, Box<dyn Error>> {
    // Just return the default builder with rustls TLS
    Ok(reqwest::Client::builder().use_rustls_tls())
}

/// Creates a reqwest client builder with FIPS-compliant TLS configuration.
/// This version loads native root certificates and verifies FIPS compliance.
#[cfg(feature = "fips")]
pub fn create_reqwest_client_builder() -> Result<ClientBuilder, Box<dyn Error>> {
    // Get the runtime crypto provider that should have been configured elsewhere in the application
    let provider =
        rustls::crypto::CryptoProvider::get_default().ok_or("No crypto provider configured")?;

    // Verify the provider is FIPS-compliant
    if !provider.fips() {
        return Err("Crypto provider is not FIPS-compliant".into());
    }

    // Create an empty root cert store
    let mut root_cert_store = rustls::RootCertStore::empty();

    // Load native certificates
    let native_certs = rustls_native_certs::load_native_certs();

    // Add the certificates to the store
    let mut valid_count = 0;

    for cert in native_certs.certs {
        match root_cert_store.add(cert) {
            Ok(()) => valid_count += 1,
            Err(err) => {
                // Optionally log errors
                debug!("Failed to parse certificate: {:?}", err);
            }
        }
    }

    // Verify we have at least some valid certificates
    if valid_count == 0 {
        return Err("No valid certificates found in native root store".into());
    }

    // Configure TLS versions (FIPS typically requires TLS 1.2 or higher)
    let versions = rustls::ALL_VERSIONS.to_vec();

    // Build the client config
    let config_builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&versions)
        .map_err(|_| "Failed to set protocol versions")?;

    // Complete the configuration without client authentication
    let config = config_builder
        .with_root_certificates(root_cert_store)
        .with_no_client_auth();

    // Verify the final config is FIPS-compliant
    if !config.fips() {
        return Err("The final TLS configuration is not FIPS-compliant".into());
    }
    debug!("Client Builder is in FIPS mode");

    // Create the reqwest client builder with our FIPS-compliant TLS configuration
    let client_builder = reqwest::Client::builder().use_preconfigured_tls(config);

    Ok(client_builder)
}
