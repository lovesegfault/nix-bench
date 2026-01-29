//! nix-bench-common - Shared types and utilities
//!
//! This crate provides shared types used by both the agent and coordinator,
//! without any AWS SDK dependencies to keep it lightweight.
//!
//! ## Modules
//!
//! - [`agent_config`]: Agent configuration for benchmark runs
//! - [`defaults`]: Default configuration values
//! - [`run_result`]: Benchmark run result type
//! - [`stats`]: Duration statistics (min/avg/max)
//! - [`status`]: Canonical status codes for gRPC communication
//! - [`tls`]: TLS certificate generation for mTLS

pub mod agent_config;
pub mod defaults;
pub mod proto;
pub mod run_result;
pub mod stats;
pub mod status;
pub mod tls;

// Re-export commonly used types
pub use agent_config::AgentConfig;
pub use run_result::RunResult;
pub use stats::DurationStats;
pub use status::StatusCode;
pub use tls::{CertKeyPair, TlsConfig};

// Re-export RunId for ergonomic use
// (defined below, after Architecture)

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// System architecture for EC2 instances.
///
/// Only two architectures are supported: x86_64 and aarch64 (Graviton).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
pub enum Architecture {
    #[serde(rename = "x86_64-linux")]
    #[strum(serialize = "x86_64-linux")]
    X86_64,
    #[serde(rename = "aarch64-linux")]
    #[strum(serialize = "aarch64-linux")]
    Aarch64,
}

impl Architecture {
    /// Detect architecture from EC2 instance type.
    ///
    /// Graviton instances have a 'g' after the generation number (e.g., c7g, m7gd).
    /// Returns `Aarch64` for Graviton, `X86_64` for all others.
    ///
    /// Instance type validity is not checked here; the coordinator validates
    /// instance types via the EC2 API (`ec2.validate_instance_types()`).
    pub fn from_instance_type(instance_type: &str) -> Self {
        let prefix = instance_type.split('.').next().unwrap_or("");

        // Detect Graviton: digit followed by 'g' in the prefix
        let chars: Vec<char> = prefix.chars().collect();
        for i in 0..chars.len().saturating_sub(1) {
            if chars[i].is_ascii_digit() && chars[i + 1] == 'g' {
                return Architecture::Aarch64;
            }
        }
        Architecture::X86_64
    }

    /// Get the string representation (e.g., "x86_64-linux").
    pub fn as_str(&self) -> &'static str {
        match self {
            Architecture::X86_64 => "x86_64-linux",
            Architecture::Aarch64 => "aarch64-linux",
        }
    }

    /// Get the AMI architecture filter string (e.g., "x86_64", "arm64").
    pub fn ami_arch(&self) -> &'static str {
        match self {
            Architecture::X86_64 => "x86_64",
            Architecture::Aarch64 => "arm64",
        }
    }
}

/// Unique identifier for a benchmark run (UUIDv7).
///
/// Wraps a UUIDv7 string to provide type safety for run identifiers.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    derive_more::Display,
    derive_more::From,
    derive_more::AsRef,
    derive_more::Deref,
)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Generate a new RunId using UUIDv7 (time-based).
    pub fn new() -> Self {
        Self(Uuid::now_v7().to_string())
    }

    /// Get the string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

/// Get the current timestamp in milliseconds since UNIX epoch.
///
/// Returns 0 if system time is before the epoch (should never happen in practice).
#[inline]
pub fn timestamp_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Install the rustls ring crypto provider as the default.
///
/// Must be called before any TLS operations. Panics if installation fails.
#[cfg(feature = "init")]
pub fn init_rustls() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");
}

/// Initialize a standard tracing subscriber with env filter and INFO default.
///
/// Suitable for CLI binaries that log to stdout. For TUI applications,
/// use a custom subscriber setup instead.
#[cfg(feature = "init")]
pub fn init_tracing_default() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_instance_type() {
        // x86_64 instances
        assert_eq!(
            Architecture::from_instance_type("c7i.metal"),
            Architecture::X86_64
        );
        assert_eq!(
            Architecture::from_instance_type("m8a.48xlarge"),
            Architecture::X86_64
        );
        assert_eq!(
            Architecture::from_instance_type("c5.xlarge"),
            Architecture::X86_64
        );

        // Graviton (aarch64) instances
        assert_eq!(
            Architecture::from_instance_type("c7g.metal"),
            Architecture::Aarch64
        );
        assert_eq!(
            Architecture::from_instance_type("m7gd.16xlarge"),
            Architecture::Aarch64
        );
        assert_eq!(
            Architecture::from_instance_type("c6gd.metal"),
            Architecture::Aarch64
        );
    }

    #[test]
    fn test_architecture_str_and_display() {
        assert_eq!(Architecture::X86_64.as_str(), "x86_64-linux");
        assert_eq!(Architecture::Aarch64.as_str(), "aarch64-linux");
        assert_eq!(format!("{}", Architecture::X86_64), "x86_64-linux");
        assert_eq!(format!("{}", Architecture::Aarch64), "aarch64-linux");
    }

    #[test]
    fn test_architecture_serde_roundtrip() {
        let x86 = Architecture::X86_64;
        let json = serde_json::to_string(&x86).unwrap();
        assert_eq!(json, "\"x86_64-linux\"");
        let back: Architecture = serde_json::from_str(&json).unwrap();
        assert_eq!(back, x86);

        let arm = Architecture::Aarch64;
        let json = serde_json::to_string(&arm).unwrap();
        assert_eq!(json, "\"aarch64-linux\"");
        let back: Architecture = serde_json::from_str(&json).unwrap();
        assert_eq!(back, arm);
    }

    #[test]
    fn test_run_id_is_uuid() {
        let id = RunId::new();
        // UUIDv7 format: 8-4-4-4-12 hex chars
        assert_eq!(id.as_str().len(), 36);
        assert!(id.as_str().contains('-'));
    }

    #[test]
    fn test_run_id_unique() {
        let a = RunId::new();
        let b = RunId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn test_run_id_display() {
        let id = RunId::new();
        let s = format!("{}", id);
        assert_eq!(s, id.as_str());
    }

    #[test]
    fn test_run_id_serde_roundtrip() {
        let id = RunId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: RunId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn test_run_id_from_string() {
        let id = RunId::from("test-123".to_string());
        assert_eq!(id.as_str(), "test-123");
    }

    #[test]
    fn test_architecture_ami_arch() {
        assert_eq!(Architecture::X86_64.ami_arch(), "x86_64");
        assert_eq!(Architecture::Aarch64.ami_arch(), "arm64");
    }
}
