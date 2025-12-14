//! nix-bench-common - Shared types and utilities
//!
//! This crate provides shared types used by both the agent and coordinator,
//! without any AWS SDK dependencies to keep it lightweight.
//!
//! ## Modules
//!
//! - [`stats`]: Duration statistics (min/avg/max)
//! - [`status`]: Canonical status codes for gRPC communication
//! - [`metrics`]: CloudWatch metric constants (namespace, dimension names)
//! - [`tls`]: TLS certificate generation for mTLS

pub mod metrics;
pub mod stats;
pub mod status;
pub mod tls;

// Re-export commonly used types
pub use stats::DurationStats;
pub use status::StatusCode;
pub use tls::{CertKeyPair, TlsConfig};
