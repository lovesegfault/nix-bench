//! Agent configuration and validation errors
//!
//! Typed errors for configuration validation, replacing `anyhow::bail!` calls.

use thiserror::Error;

/// Configuration validation errors
#[derive(Debug, Error)]
pub enum ConfigError {
    /// run_id field is empty
    #[error("run_id cannot be empty")]
    EmptyRunId,

    /// bucket field is empty
    #[error("bucket cannot be empty")]
    EmptyBucket,

    /// region field is empty
    #[error("region cannot be empty")]
    EmptyRegion,

    /// attr field is empty
    #[error("attr cannot be empty")]
    EmptyAttr,

    /// runs field is zero
    #[error("runs must be at least 1, got {0}")]
    InvalidRuns(u32),

    /// instance_type field is empty
    #[error("instance_type cannot be empty")]
    EmptyInstanceType,

    /// system field has invalid value
    #[error("system must be 'x86_64-linux' or 'aarch64-linux', got: {0}")]
    InvalidSystem(String),

    /// flake_ref field is empty
    #[error("flake_ref cannot be empty")]
    EmptyFlakeRef,

    /// build_timeout is zero
    #[error("build_timeout must be greater than 0")]
    InvalidBuildTimeout,

    /// max_failures is zero
    #[error("max_failures must be at least 1")]
    InvalidMaxFailures,

    /// broadcast_capacity is zero
    #[error("broadcast_capacity must be at least 1")]
    InvalidBroadcastCapacity,

    /// TLS required but CA cert missing
    #[error("require_tls is true but ca_cert_pem is not provided")]
    MissingCaCert,

    /// TLS required but agent cert missing
    #[error("require_tls is true but agent_cert_pem is not provided")]
    MissingAgentCert,

    /// TLS required but agent key missing
    #[error("require_tls is true but agent_key_pem is not provided")]
    MissingAgentKey,

    /// Failed to parse JSON configuration
    #[error("Failed to parse config: {0}")]
    Parse(#[from] serde_json::Error),

    /// Failed to read configuration file
    #[error("Failed to read config file '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl ConfigError {
    /// Create an IO error with path context
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        assert_eq!(ConfigError::EmptyRunId.to_string(), "run_id cannot be empty");
        assert_eq!(
            ConfigError::InvalidRuns(0).to_string(),
            "runs must be at least 1, got 0"
        );
        assert_eq!(
            ConfigError::InvalidSystem("windows".to_string()).to_string(),
            "system must be 'x86_64-linux' or 'aarch64-linux', got: windows"
        );
    }

    #[test]
    fn test_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = ConfigError::io("/path/to/config.json", io_err);
        assert!(err.to_string().contains("/path/to/config.json"));
    }
}
