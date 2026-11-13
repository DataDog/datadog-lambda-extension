//! An in-memory certificate authority for the MITM proxy.

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, SanType,
};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// System trust stores, in the order Amazon Linux and Debian lay them out.
const SYSTEM_BUNDLES: &[&str] = &[
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/cert.pem",
];

pub struct CertificateAuthority {
    cert: Certificate,
    key: KeyPair,
    configs: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl CertificateAuthority {
    pub fn generate() -> Result<Self, rcgen::Error> {
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "Datadog Lambda Replay CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let key = KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        Ok(Self {
            cert,
            key,
            configs: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn cert_pem(&self) -> String {
        self.cert.pem()
    }

    /// Writes `ca.pem` (the CA alone, for `NODE_EXTRA_CA_CERTS`) and
    /// `ca-bundle.pem` (system roots plus the CA, for variables that replace
    /// the trust store).
    pub fn write_files(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let pem = self.cert_pem();
        std::fs::write(dir.join("ca.pem"), &pem)?;
        let mut bundle = SYSTEM_BUNDLES
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        if !bundle.ends_with('\n') && !bundle.is_empty() {
            bundle.push('\n');
        }
        bundle.push_str(&pem);
        std::fs::write(dir.join("ca-bundle.pem"), bundle)
    }

    /// A TLS server config presenting a leaf certificate for `host`, cached per host.
    pub fn server_config(&self, host: &str) -> Result<Arc<ServerConfig>, String> {
        let mut configs = self.configs.lock().map_err(|e| e.to_string())?;
        if let Some(c) = configs.get(host) {
            return Ok(Arc::clone(c));
        }
        let config = Arc::new(self.mint(host)?);
        configs.insert(host.to_string(), Arc::clone(&config));
        Ok(config)
    }

    fn mint(&self, host: &str) -> Result<ServerConfig, String> {
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, host);
        params.subject_alt_names = vec![match host.parse::<std::net::IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(host.try_into().map_err(|e| format!("{e}"))?),
        }];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2034, 1, 1);
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let leaf = params
            .signed_by(&key, &self.cert, &self.key)
            .map_err(|e| e.to_string())?;

        let chain = vec![
            leaf.der().clone(),
            CertificateDer::from(self.cert.der().to_vec()),
        ];
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key_der)
            .map_err(|e| e.to_string())?;
        // Clients then speak HTTP/1.1 inside the tunnel, which the proxy serves.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_and_caches_leaf_configs() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ca = CertificateAuthority::generate().expect("ca");
        assert!(ca.cert_pem().starts_with("-----BEGIN CERTIFICATE-----"));
        let a = ca
            .server_config("dynamodb.us-east-1.amazonaws.com")
            .expect("config");
        let b = ca
            .server_config("dynamodb.us-east-1.amazonaws.com")
            .expect("config");
        assert!(Arc::ptr_eq(&a, &b));
        ca.server_config("127.0.0.1").expect("ip config");
    }
}
