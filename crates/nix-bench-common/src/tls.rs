//! TLS certificate generation for mTLS between coordinator and agents
//!
//! This module provides ephemeral CA and certificate generation for securing
//! gRPC communication. The coordinator generates a CA and per-agent certificates
//! at runtime, which are distributed via S3 along with the agent config.

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};

/// Parse a CA certificate and key from PEM to create an Issuer for signing.
fn parse_ca_for_signing(ca_cert_pem: &str, ca_key_pem: &str) -> Result<Issuer<'static, KeyPair>> {
    let ca_key_pair = KeyPair::from_pem(ca_key_pem).context("Failed to parse CA private key")?;

    Issuer::from_ca_cert_pem(ca_cert_pem, ca_key_pair)
        .context("Failed to create issuer from CA certificate")
}

/// PEM-encoded certificate and private key pair
#[derive(Debug, Clone)]
pub struct CertKeyPair {
    pub cert_pem: String,
    pub key_pem: String,
}

/// TLS configuration for gRPC
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// CA certificate (PEM)
    pub ca_cert_pem: String,
    /// Server/client certificate (PEM)
    pub cert_pem: String,
    /// Server/client private key (PEM)
    pub key_pem: String,
}

impl TlsConfig {
    /// Create a tonic Identity from the cert/key pair

    pub fn identity(&self) -> Result<tonic::transport::Identity> {
        Ok(tonic::transport::Identity::from_pem(
            self.cert_pem.as_bytes(),
            self.key_pem.as_bytes(),
        ))
    }

    /// Create a tonic Certificate from the CA cert

    pub fn ca_certificate(&self) -> Result<tonic::transport::Certificate> {
        Ok(tonic::transport::Certificate::from_pem(
            self.ca_cert_pem.as_bytes(),
        ))
    }

    /// Configure a tonic server with TLS (for agent)

    pub fn server_tls_config(&self) -> Result<tonic::transport::ServerTlsConfig> {
        let identity = self.identity()?;
        let ca_cert = self.ca_certificate()?;

        Ok(tonic::transport::ServerTlsConfig::new()
            .identity(identity)
            .client_ca_root(ca_cert))
    }

    /// Configure a tonic client with TLS (for coordinator)

    pub fn client_tls_config(&self) -> Result<tonic::transport::ClientTlsConfig> {
        let identity = self.identity()?;
        let ca_cert = self.ca_certificate()?;

        Ok(tonic::transport::ClientTlsConfig::new()
            .identity(identity)
            .ca_certificate(ca_cert)
            .domain_name("nix-bench-agent"))
    }
}

/// Generate a self-signed CA certificate for signing agent/coordinator certs.
/// Returns the CA cert/key pair as PEMs.
pub fn generate_ca(run_id: &str) -> Result<CertKeyPair> {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, format!("nix-bench-ca-{}", run_id));
    params
        .distinguished_name
        .push(DnType::OrganizationName, "nix-bench");

    // CA certificate settings
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];

    // rcgen uses default validity (not_before = now, not_after = now + 1 year by default)
    // which is fine for ephemeral certs

    let key_pair = KeyPair::generate().context("Failed to generate CA key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("Failed to generate CA certificate")?;

    Ok(CertKeyPair {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
    })
}

/// Generate a signed certificate with the given parameters.
fn generate_signed_cert(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    cn: &str,
    ou: Option<&str>,
    key_usage: ExtendedKeyUsagePurpose,
    sans: Vec<SanType>,
) -> Result<CertKeyPair> {
    let issuer = parse_ca_for_signing(ca_cert_pem, ca_key_pem)?;

    let mut params = CertificateParams::default();
    params.distinguished_name.push(DnType::CommonName, cn);
    params
        .distinguished_name
        .push(DnType::OrganizationName, "nix-bench");
    if let Some(ou) = ou {
        params
            .distinguished_name
            .push(DnType::OrganizationalUnitName, ou);
    }

    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![key_usage];
    params.subject_alt_names = sans;

    let key_pair = KeyPair::generate().context("Failed to generate key pair")?;
    let cert = params
        .signed_by(&key_pair, &issuer)
        .context("Failed to sign certificate")?;

    Ok(CertKeyPair {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
    })
}

/// Generate a server certificate for an agent, signed by the CA.
pub fn generate_agent_cert(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    instance_type: &str,
    public_ip: Option<&str>,
) -> Result<CertKeyPair> {
    let mut sans = vec![SanType::DnsName("nix-bench-agent".try_into().unwrap())];
    if let Some(ip) = public_ip {
        if let Ok(ip_addr) = ip.parse() {
            sans.push(SanType::IpAddress(ip_addr));
        }
    }

    generate_signed_cert(
        ca_cert_pem,
        ca_key_pem,
        "nix-bench-agent",
        Some(&format!("agent-{}", instance_type)),
        ExtendedKeyUsagePurpose::ServerAuth,
        sans,
    )
}

/// Generate a client certificate for the coordinator, signed by the CA.
pub fn generate_coordinator_cert(ca_cert_pem: &str, ca_key_pem: &str) -> Result<CertKeyPair> {
    generate_signed_cert(
        ca_cert_pem,
        ca_key_pem,
        "nix-bench-coordinator",
        None,
        ExtendedKeyUsagePurpose::ClientAuth,
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_ca() {
        let ca = generate_ca("test-run-123").unwrap();
        assert!(ca.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_generate_agent_cert() {
        let ca = generate_ca("test-run-123").unwrap();
        let agent_cert =
            generate_agent_cert(&ca.cert_pem, &ca.key_pem, "c6i.xlarge", Some("10.0.0.1")).unwrap();

        assert!(agent_cert.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(agent_cert.key_pem.contains("BEGIN PRIVATE KEY"));
        // Agent cert should be different from CA cert
        assert_ne!(agent_cert.cert_pem, ca.cert_pem);
    }

    #[test]
    fn test_generate_coordinator_cert() {
        let ca = generate_ca("test-run-123").unwrap();
        let coord_cert = generate_coordinator_cert(&ca.cert_pem, &ca.key_pem).unwrap();

        assert!(coord_cert.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(coord_cert.key_pem.contains("BEGIN PRIVATE KEY"));
        // Coordinator cert should be different from CA cert
        assert_ne!(coord_cert.cert_pem, ca.cert_pem);
    }

    #[test]
    fn test_tls_config_creation() {
        let ca = generate_ca("test-run-123").unwrap();
        let agent_cert =
            generate_agent_cert(&ca.cert_pem, &ca.key_pem, "c6i.xlarge", None).unwrap();

        let config = TlsConfig {
            ca_cert_pem: ca.cert_pem,
            cert_pem: agent_cert.cert_pem,
            key_pem: agent_cert.key_pem,
        };

        // Just verify the struct can be created
        assert!(!config.ca_cert_pem.is_empty());
        assert!(!config.cert_pem.is_empty());
        assert!(!config.key_pem.is_empty());
    }
}
