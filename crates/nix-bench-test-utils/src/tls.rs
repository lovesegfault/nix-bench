//! TLS test utilities
//!
//! Provides crypto provider initialization and TLS certificate generation helpers.

use nix_bench_common::tls::{
    generate_agent_cert, generate_ca, generate_coordinator_cert, TlsConfig,
};
use std::sync::Once;

/// Default test run ID for TLS certificates
pub const TEST_RUN_ID: &str = "test-run-123";

/// Default test instance type for TLS certificates
pub const TEST_INSTANCE_TYPE: &str = "c6i.xlarge";

/// Install the rustls crypto provider (once per process)
static INIT: Once = Once::new();

/// Initialize crypto provider for TLS tests.
///
/// This must be called before any TLS operations. It's safe to call
/// multiple times - only the first call has any effect.
///
/// # Example
///
/// ```
/// use nix_bench_test_utils::tls::init_crypto;
///
/// init_crypto();
/// // Now TLS operations will work
/// ```
pub fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider");
    });
}

/// Test TLS configuration for agent and coordinator
pub struct TestTlsCerts {
    /// TLS config for the agent (server side)
    pub agent_tls: TlsConfig,
    /// TLS config for the coordinator (client side)
    pub coordinator_tls: TlsConfig,
}

/// Generate test TLS certificates for integration tests.
///
/// Creates a CA, agent certificate, and coordinator certificate
/// suitable for local testing with 127.0.0.1.
///
/// # Example
///
/// ```
/// use nix_bench_test_utils::tls::generate_test_certs;
///
/// let certs = generate_test_certs();
/// // Use certs.agent_tls for server, certs.coordinator_tls for client
/// ```
pub fn generate_test_certs() -> TestTlsCerts {
    generate_test_certs_for_ip("127.0.0.1")
}

/// Generate test TLS certificates for a specific IP address.
///
/// # Arguments
///
/// * `ip` - The IP address to include in the agent certificate's SAN
pub fn generate_test_certs_for_ip(ip: &str) -> TestTlsCerts {
    init_crypto();
    let ca = generate_ca("test-integration").expect("Failed to generate CA");

    let agent_cert = generate_agent_cert(&ca.cert_pem, &ca.key_pem, TEST_INSTANCE_TYPE, Some(ip))
        .expect("Failed to generate agent cert");

    let coord_cert = generate_coordinator_cert(&ca.cert_pem, &ca.key_pem)
        .expect("Failed to generate coordinator cert");

    TestTlsCerts {
        agent_tls: TlsConfig {
            ca_cert_pem: ca.cert_pem.clone(),
            cert_pem: agent_cert.cert_pem,
            key_pem: agent_cert.key_pem,
        },
        coordinator_tls: TlsConfig {
            ca_cert_pem: ca.cert_pem,
            cert_pem: coord_cert.cert_pem,
            key_pem: coord_cert.key_pem,
        },
    }
}

/// Generate test TLS certificates for multiple instance types.
///
/// Useful for E2E tests that spin up multiple instances of different types.
///
/// # Arguments
///
/// * `instances` - List of (instance_type, ip_address) pairs
///
/// # Returns
///
/// A tuple of (agent_certs_map, coordinator_tls) where agent_certs_map
/// maps instance_type -> TlsConfig
pub fn generate_multi_instance_certs(
    instances: &[(&str, &str)],
) -> (std::collections::HashMap<String, TlsConfig>, TlsConfig) {
    init_crypto();
    let ca = generate_ca("test-multi-instance").expect("Failed to generate CA");

    let coord_cert = generate_coordinator_cert(&ca.cert_pem, &ca.key_pem)
        .expect("Failed to generate coordinator cert");
    let coordinator_tls = TlsConfig {
        ca_cert_pem: ca.cert_pem.clone(),
        cert_pem: coord_cert.cert_pem,
        key_pem: coord_cert.key_pem,
    };

    let mut agent_certs = std::collections::HashMap::new();
    for (instance_type, ip) in instances {
        let agent_cert = generate_agent_cert(&ca.cert_pem, &ca.key_pem, instance_type, Some(ip))
            .expect("Failed to generate agent cert");
        agent_certs.insert(
            instance_type.to_string(),
            TlsConfig {
                ca_cert_pem: ca.cert_pem.clone(),
                cert_pem: agent_cert.cert_pem,
                key_pem: agent_cert.key_pem,
            },
        );
    }

    (agent_certs, coordinator_tls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_crypto_idempotent() {
        // Should be safe to call multiple times
        init_crypto();
        init_crypto();
        init_crypto();
    }

    #[test]
    fn test_generate_test_certs() {
        let certs = generate_test_certs();
        assert!(!certs.agent_tls.cert_pem.is_empty());
        assert!(!certs.coordinator_tls.cert_pem.is_empty());
    }

    #[test]
    fn test_generate_multi_instance_certs() {
        let instances = [("c7a.medium", "1.2.3.4"), ("c7g.medium", "5.6.7.8")];
        let (agent_certs, coordinator_tls) = generate_multi_instance_certs(&instances);

        assert_eq!(agent_certs.len(), 2);
        assert!(agent_certs.contains_key("c7a.medium"));
        assert!(agent_certs.contains_key("c7g.medium"));
        assert!(!coordinator_tls.cert_pem.is_empty());
    }
}
