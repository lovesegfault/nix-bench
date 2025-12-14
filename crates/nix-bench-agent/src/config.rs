//! Configuration loading from JSON

use crate::error::ConfigError;
use anyhow::{Context, Result};
use nix_bench_common::defaults::{default_build_timeout, default_flake_ref, default_max_failures};
use nix_bench_common::TlsConfig;
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Default gc_between_runs setting
fn default_gc_between_runs() -> bool {
    false
}

/// Agent configuration loaded from JSON
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Unique run identifier (UUIDv7)
    pub run_id: String,

    /// S3 bucket for results
    pub bucket: String,

    /// AWS region
    pub region: String,

    /// nix-bench attribute to build (e.g., "large-deep")
    pub attr: String,

    /// Number of benchmark runs
    pub runs: u32,

    /// EC2 instance type (for metrics dimensions)
    pub instance_type: String,

    /// System architecture (e.g., "x86_64-linux")
    pub system: String,

    /// Flake reference base (e.g., "github:lovesegfault/nix-bench")
    /// Defaults to the official nix-bench repository
    #[serde(default = "default_flake_ref")]
    pub flake_ref: String,

    /// Build timeout in seconds (default: 7200 = 2 hours)
    #[serde(default = "default_build_timeout")]
    pub build_timeout: u64,

    /// Maximum number of build failures before giving up (default: 3)
    #[serde(default = "default_max_failures")]
    pub max_failures: u32,

    /// Run garbage collection between benchmark runs (default: false)
    /// This preserves fixed-output derivations (fetched sources) but removes build outputs,
    /// keeping disk usage manageable during long benchmark sessions.
    #[serde(default = "default_gc_between_runs")]
    pub gc_between_runs: bool,

    /// CA certificate (PEM) for mTLS
    #[serde(default)]
    pub ca_cert_pem: Option<String>,

    /// Agent certificate (PEM) for mTLS
    #[serde(default)]
    pub agent_cert_pem: Option<String>,

    /// Agent private key (PEM) for mTLS
    #[serde(default)]
    pub agent_key_pem: Option<String>,
}

impl Config {
    /// Load configuration from a JSON file
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        config.validate()?;
        Ok(config)
    }

    /// Validate configuration values (called automatically by load())
    fn validate(&self) -> Result<(), ConfigError> {
        self.validate_full()
    }

    /// Validate all configuration values including TLS certificates.
    ///
    /// This is public so it can be called after downloading config from S3.
    pub fn validate_full(&self) -> Result<(), ConfigError> {
        if self.run_id.is_empty() {
            return Err(ConfigError::EmptyRunId);
        }

        if self.bucket.is_empty() {
            return Err(ConfigError::EmptyBucket);
        }

        if self.region.is_empty() {
            return Err(ConfigError::EmptyRegion);
        }

        if self.attr.is_empty() {
            return Err(ConfigError::EmptyAttr);
        }

        if self.runs == 0 {
            return Err(ConfigError::InvalidRuns(self.runs));
        }

        if self.instance_type.is_empty() {
            return Err(ConfigError::EmptyInstanceType);
        }

        if self.system != "x86_64-linux" && self.system != "aarch64-linux" {
            return Err(ConfigError::InvalidSystem(self.system.clone()));
        }

        if self.flake_ref.is_empty() {
            return Err(ConfigError::EmptyFlakeRef);
        }

        if self.build_timeout == 0 {
            return Err(ConfigError::InvalidBuildTimeout);
        }

        if self.max_failures == 0 {
            return Err(ConfigError::InvalidMaxFailures);
        }

        // TLS certificates are always required for security
        if self.ca_cert_pem.is_none() {
            return Err(ConfigError::MissingCaCert);
        }
        if self.agent_cert_pem.is_none() {
            return Err(ConfigError::MissingAgentCert);
        }
        if self.agent_key_pem.is_none() {
            return Err(ConfigError::MissingAgentKey);
        }

        Ok(())
    }

    /// Get TLS configuration (required)
    ///
    /// # Errors
    /// Returns an error if any TLS certificate is missing.
    /// This should not happen if `validate()` was called first.
    pub fn tls_config(&self) -> Result<TlsConfig, ConfigError> {
        let ca = self.ca_cert_pem.as_ref().ok_or(ConfigError::MissingCaCert)?;
        let cert = self.agent_cert_pem.as_ref().ok_or(ConfigError::MissingAgentCert)?;
        let key = self.agent_key_pem.as_ref().ok_or(ConfigError::MissingAgentKey)?;

        Ok(TlsConfig {
            ca_cert_pem: ca.clone(),
            cert_pem: cert.clone(),
            key_pem: key.clone(),
        })
    }
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

        let config = Config::load(file.path()).unwrap();
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

        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.flake_ref, "github:myorg/my-bench");
        assert_eq!(config.build_timeout, 3600);
        assert_eq!(config.max_failures, 5);
        assert_eq!(config.system, "aarch64-linux");
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

        let result = Config::load(file.path());
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

        let result = Config::load(file.path());
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

        let config = Config::load(file.path()).unwrap();
        let tls_config = config.tls_config();
        assert!(tls_config.is_ok());
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

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("ca_cert_pem"));
    }
}
