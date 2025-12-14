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
//! - [`tags`]: AWS resource tag constants for discovery and cleanup
//! - [`tls`]: TLS certificate generation for mTLS

pub mod agent_config;
pub mod defaults;
pub mod resource_kind;
pub mod run_result;
pub mod stats;
pub mod status;
pub mod tags;
pub mod tls;

// Re-export commonly used types
pub use agent_config::AgentConfig;
pub use resource_kind::ResourceKind;
pub use run_result::RunResult;
pub use stats::DurationStats;
pub use status::StatusCode;
pub use tls::{CertKeyPair, TlsConfig};

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
