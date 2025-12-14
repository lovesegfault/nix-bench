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

    /// TLS CA cert missing (required for mTLS)
    #[error("TLS is required but ca_cert_pem is not provided")]
    MissingCaCert,

    /// TLS agent cert missing (required for mTLS)
    #[error("TLS is required but agent_cert_pem is not provided")]
    MissingAgentCert,

    /// TLS agent key missing (required for mTLS)
    #[error("TLS is required but agent_key_pem is not provided")]
    MissingAgentKey,

    /// Failed to parse JSON configuration
    #[error("Failed to parse config: {0}")]
    Parse(#[from] serde_json::Error),
}
