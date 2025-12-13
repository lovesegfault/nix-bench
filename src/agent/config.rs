//! Configuration loading from JSON

use crate::agent::error::ConfigError;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Default flake reference for nix-bench
fn default_flake_ref() -> String {
    "github:lovesegfault/nix-bench".to_string()
}

/// Default build timeout in seconds (2 hours)
fn default_build_timeout() -> u64 {
    7200
}

/// Default max failures before giving up
fn default_max_failures() -> u32 {
    3
}

/// Default broadcast channel capacity for gRPC log streaming
fn default_broadcast_capacity() -> usize {
    1024
}

/// Default gRPC port
fn default_grpc_port() -> u16 {
    50051
}

/// Default require_tls setting
fn default_require_tls() -> bool {
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

    /// Broadcast channel capacity for gRPC log streaming (default: 1024)
    #[serde(default = "default_broadcast_capacity")]
    pub broadcast_capacity: usize,

    /// gRPC server port (default: 50051)
    #[serde(default = "default_grpc_port")]
    pub grpc_port: u16,

    /// Require TLS for gRPC server (default: false)
    /// If true and TLS certificates are not provided, the agent will fail to start
    #[serde(default = "default_require_tls")]
    pub require_tls: bool,

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

    /// Validate configuration values
    fn validate(&self) -> Result<(), ConfigError> {
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

        if self.broadcast_capacity == 0 {
            return Err(ConfigError::InvalidBroadcastCapacity);
        }

        // If require_tls is true, all TLS certificates must be provided
        if self.require_tls {
            if self.ca_cert_pem.is_none() {
                return Err(ConfigError::MissingCaCert);
            }
            if self.agent_cert_pem.is_none() {
                return Err(ConfigError::MissingAgentCert);
            }
            if self.agent_key_pem.is_none() {
                return Err(ConfigError::MissingAgentKey);
            }
        }

        Ok(())
    }

    /// Get TLS configuration if certificates are present
    pub fn tls_config(&self) -> Option<crate::tls::TlsConfig> {
        match (&self.ca_cert_pem, &self.agent_cert_pem, &self.agent_key_pem) {
            (Some(ca), Some(cert), Some(key)) => Some(crate::tls::TlsConfig {
                ca_cert_pem: ca.clone(),
                cert_pem: cert.clone(),
                key_pem: key.clone(),
            }),
            _ => None,
        }
    }

    /// Check if TLS is enabled
    pub fn tls_enabled(&self) -> bool {
        self.ca_cert_pem.is_some()
            && self.agent_cert_pem.is_some()
            && self.agent_key_pem.is_some()
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
                "system": "x86_64-linux"
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
                "max_failures": 5
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
                "system": "x86_64-linux"
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
                "system": "x86_64-linux"
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("runs"));
    }

    #[test]
    fn test_validation_invalid_system() {
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
                "system": "windows"
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("system"));
    }

    #[test]
    fn test_load_missing_file() {
        let result = Config::load(Path::new("/nonexistent/config.json"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to read"));
    }

    #[test]
    fn test_validation_empty_bucket() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "",
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
        assert!(result.unwrap_err().to_string().contains("bucket"));
    }

    #[test]
    fn test_validation_empty_region() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "",
                "attr": "large-deep",
                "runs": 10,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux"
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("region"));
    }

    #[test]
    fn test_validation_empty_attr() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "",
                "runs": 10,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux"
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("attr"));
    }

    #[test]
    fn test_validation_empty_instance_type() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 10,
                "instance_type": "",
                "system": "x86_64-linux"
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("instance_type"));
    }

    #[test]
    fn test_validation_empty_flake_ref() {
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
                "flake_ref": ""
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("flake_ref"));
    }

    #[test]
    fn test_validation_zero_build_timeout() {
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
                "build_timeout": 0
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("build_timeout"));
    }

    #[test]
    fn test_validation_zero_max_failures() {
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
                "max_failures": 0
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("max_failures"));
    }

    #[test]
    fn test_validation_zero_broadcast_capacity() {
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
                "broadcast_capacity": 0
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("broadcast_capacity"));
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
        assert!(config.tls_enabled());
        let tls_config = config.tls_config();
        assert!(tls_config.is_some());
    }

    #[test]
    fn test_tls_config_when_certs_missing() {
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

        let config = Config::load(file.path()).unwrap();
        assert!(!config.tls_enabled());
        assert!(config.tls_config().is_none());
    }

    #[test]
    fn test_tls_config_when_partial_certs() {
        // Only CA cert, missing agent cert and key
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
                "ca_cert_pem": "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----"
            }}"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        // Partial TLS config should not be enabled
        assert!(!config.tls_enabled());
        assert!(config.tls_config().is_none());
    }

    #[test]
    fn test_require_tls_without_certs_fails() {
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
                "require_tls": true
            }}"#
        )
        .unwrap();

        let result = Config::load(file.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("require_tls is true but ca_cert_pem is not provided"));
    }

    #[test]
    fn test_require_tls_with_certs_succeeds() {
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
                "require_tls": true,
                "ca_cert_pem": "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
                "agent_cert_pem": "-----BEGIN CERTIFICATE-----\nAGENT\n-----END CERTIFICATE-----",
                "agent_key_pem": "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----"
            }}"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert!(config.require_tls);
        assert!(config.tls_enabled());
    }
}
