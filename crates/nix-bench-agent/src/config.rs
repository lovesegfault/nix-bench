//! Configuration loading and validation

use anyhow::{Context, Result, anyhow};
use garde::Validate;
use nix_bench_common::TlsConfig;
use std::fs;
use std::path::Path;

// Re-export AgentConfig as Config for this crate
pub use nix_bench_common::AgentConfig as Config;

/// Load configuration from a JSON file
pub fn load_config(path: &Path) -> Result<Config> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;

    let config: Config = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

    config
        .validate()
        .map_err(|e| anyhow!("Config validation failed: {e}"))?;
    Ok(config)
}

/// Validate all configuration values including TLS certificates.
pub fn validate_config(config: &Config) -> Result<()> {
    config
        .validate()
        .map_err(|e| anyhow!("Config validation failed: {e}"))
}

/// Get TLS configuration from config (required)
///
/// # Errors
/// Returns an error if any TLS certificate is missing.
pub fn get_tls_config(config: &Config) -> Result<TlsConfig> {
    let ca = config
        .ca_cert_pem
        .as_ref()
        .ok_or_else(|| anyhow!("TLS is required but ca_cert_pem is not provided"))?;
    let cert = config
        .agent_cert_pem
        .as_ref()
        .ok_or_else(|| anyhow!("TLS is required but agent_cert_pem is not provided"))?;
    let key = config
        .agent_key_pem
        .as_ref()
        .ok_or_else(|| anyhow!("TLS is required but agent_key_pem is not provided"))?;

    Ok(TlsConfig {
        ca_cert_pem: ca.clone(),
        cert_pem: cert.clone(),
        key_pem: key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_config() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 10,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux",
                "ca_cert_pem": "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
                "agent_cert_pem": "-----BEGIN CERTIFICATE-----\nAGENT\n-----END CERTIFICATE-----",
                "agent_key_pem": "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----"
            }}"#
        )
        .unwrap();

        let config = load_config(file.path()).unwrap();
        assert_eq!(config.run_id, "abc123");
        assert_eq!(config.runs, 10);
        assert_eq!(config.attr, "large-deep");
        // Verify defaults are applied
        assert_eq!(config.flake_ref, "github:lovesegfault/nix-bench");
        assert_eq!(config.build_timeout, 7200);
        assert_eq!(config.max_failures, 3);
    }

    #[test]
    fn test_load_config_with_custom_fields() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 5,
                "instance_type": "c7id.metal",
                "system": "aarch64-linux",
                "flake_ref": "github:myorg/my-bench",
                "build_timeout": 3600,
                "max_failures": 5,
                "ca_cert_pem": "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
                "agent_cert_pem": "-----BEGIN CERTIFICATE-----\nAGENT\n-----END CERTIFICATE-----",
                "agent_key_pem": "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----"
            }}"#
        )
        .unwrap();

        let config = load_config(file.path()).unwrap();
        assert_eq!(config.flake_ref, "github:myorg/my-bench");
        assert_eq!(config.build_timeout, 3600);
        assert_eq!(config.max_failures, 5);
        assert_eq!(config.system, nix_bench_common::Architecture::Aarch64);
    }

    #[test]
    fn test_validation_empty_run_id() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 10,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux",
                "ca_cert_pem": "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
                "agent_cert_pem": "-----BEGIN CERTIFICATE-----\nAGENT\n-----END CERTIFICATE-----",
                "agent_key_pem": "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----"
            }}"#
        )
        .unwrap();

        let result = load_config(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("run_id"));
    }

    #[test]
    fn test_validation_zero_runs() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 0,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux",
                "ca_cert_pem": "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
                "agent_cert_pem": "-----BEGIN CERTIFICATE-----\nAGENT\n-----END CERTIFICATE-----",
                "agent_key_pem": "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----"
            }}"#
        )
        .unwrap();

        let result = load_config(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("runs"));
    }

    #[test]
    fn test_tls_config_when_all_certs_present() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 10,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux",
                "ca_cert_pem": "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
                "agent_cert_pem": "-----BEGIN CERTIFICATE-----\nAGENT\n-----END CERTIFICATE-----",
                "agent_key_pem": "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----"
            }}"#
        )
        .unwrap();

        let config = load_config(file.path()).unwrap();
        let tls = get_tls_config(&config);
        assert!(tls.is_ok());
    }

    #[test]
    fn test_missing_tls_certs_fails() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 10,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux"
            }}"#
        )
        .unwrap();

        let result = load_config(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("ca_cert_pem"));
    }
}
