//! TLS certificate generation for mTLS between coordinator and agents
//!
//! This module provides ephemeral CA and certificate generation for securing
//! gRPC communication. The coordinator generates a CA and per-agent certificates
//! at runtime, which are distributed via S3 along with the agent config.

use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};

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
    #[cfg(any(feature = "agent", feature = "coordinator"))]
    pub fn identity(&self) -> Result<tonic::transport::Identity> {
        Ok(tonic::transport::Identity::from_pem(
            self.cert_pem.as_bytes(),
            self.key_pem.as_bytes(),
        ))
    }

    /// Create a tonic Certificate from the CA cert
    #[cfg(any(feature = "agent", feature = "coordinator"))]
    pub fn ca_certificate(&self) -> Result<tonic::transport::Certificate> {
        Ok(tonic::transport::Certificate::from_pem(
            self.ca_cert_pem.as_bytes(),
        ))
    }

    /// Configure a tonic server with TLS
    #[cfg(feature = "agent")]
    pub fn server_tls_config(&self) -> Result<tonic::transport::ServerTlsConfig> {
        let identity = self.identity()?;
        let ca_cert = self.ca_certificate()?;

        Ok(tonic::transport::ServerTlsConfig::new()
            .identity(identity)
            .client_ca_root(ca_cert))
    }

    /// Configure a tonic client with TLS
    #[cfg(feature = "coordinator")]
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

/// Generate a server certificate for an agent, signed by the CA.
///
/// # Arguments
/// * `ca_cert_pem` - The CA certificate PEM
/// * `ca_key_pem` - The CA private key PEM
/// * `instance_type` - Instance type for the OU field
/// * `public_ip` - Optional public IP to add as SAN
pub fn generate_agent_cert(
    ca_cert_pem: &str,
    ca_key_pem: &str,
    instance_type: &str,
    public_ip: Option<&str>,
) -> Result<CertKeyPair> {
    // Parse the CA key pair
    let ca_key_pair = KeyPair::from_pem(ca_key_pem).context("Failed to parse CA private key")?;

    // Parse the CA certificate from PEM to get its parameters
    let ca_cert_der = pem::parse(ca_cert_pem)
        .context("Failed to parse CA certificate PEM")?
        .into_contents();
    let ca_params = CertificateParams::from_ca_cert_der(&ca_cert_der.into())
        .context("Failed to parse CA certificate parameters")?;

    let ca_cert = ca_params
        .self_signed(&ca_key_pair)
        .context("Failed to recreate CA certificate for signing")?;

    // Now create the agent certificate
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "nix-bench-agent");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "nix-bench");
    params.distinguished_name.push(
        DnType::OrganizationalUnitName,
        format!("agent-{}", instance_type),
    );

    // Server certificate settings
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    // Add SANs for connection
    params.subject_alt_names = vec![SanType::DnsName("nix-bench-agent".try_into().unwrap())];
    if let Some(ip) = public_ip {
        if let Ok(ip_addr) = ip.parse() {
            params.subject_alt_names.push(SanType::IpAddress(ip_addr));
        }
    }

    let key_pair = KeyPair::generate().context("Failed to generate agent key pair")?;
    let cert = params
        .signed_by(&key_pair, &ca_cert, &ca_key_pair)
        .context("Failed to sign agent certificate")?;

    // We ignore ca_cert_pem since we recreated the CA from the key
    let _ = ca_cert_pem;

    Ok(CertKeyPair {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
    })
}

/// Generate a client certificate for the coordinator, signed by the CA.
///
/// # Arguments
/// * `ca_cert_pem` - The CA certificate PEM
/// * `ca_key_pem` - The CA private key PEM
pub fn generate_coordinator_cert(ca_cert_pem: &str, ca_key_pem: &str) -> Result<CertKeyPair> {
    // Parse the CA key pair
    let ca_key_pair = KeyPair::from_pem(ca_key_pem).context("Failed to parse CA private key")?;

    // Parse the CA certificate from PEM to get its parameters
    let ca_cert_der = pem::parse(ca_cert_pem)
        .context("Failed to parse CA certificate PEM")?
        .into_contents();
    let ca_params = CertificateParams::from_ca_cert_der(&ca_cert_der.into())
        .context("Failed to parse CA certificate parameters")?;

    let ca_cert = ca_params
        .self_signed(&ca_key_pair)
        .context("Failed to recreate CA certificate for signing")?;

    // Now create the coordinator certificate
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, "nix-bench-coordinator");
    params
        .distinguished_name
        .push(DnType::OrganizationName, "nix-bench");

    // Client certificate settings
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];

    let key_pair = KeyPair::generate().context("Failed to generate coordinator key pair")?;
    let cert = params
        .signed_by(&key_pair, &ca_cert, &ca_key_pair)
        .context("Failed to sign coordinator certificate")?;

    // We ignore ca_cert_pem since we recreated the CA from the key
    let _ = ca_cert_pem;

    Ok(CertKeyPair {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
    })
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
