//! nix-bench - Nix build benchmarking toolkit
//!
//! This crate provides both the coordinator and agent binaries for running
//! Nix build benchmarks on EC2 instances.
//!
//! ## Binaries
//!
//! - `nix-bench-coordinator`: TUI-based orchestrator that launches EC2 instances
//! - `nix-bench-agent`: Runs on EC2 instances to execute benchmarks
//!
//! ## Shared Modules
//!
//! - `metrics`: Constants for CloudWatch metrics dimensions
//! - `stats`: Duration statistics utility (min/avg/max)
//! - `status`: Canonical status codes for agent/coordinator communication
//! - `tls`: TLS certificate generation for mTLS
//! - `aws_context`: Shared AWS SDK configuration

pub mod aws_context;
pub mod metrics;
pub mod stats;
pub mod status;
pub mod tls;

// Test fixtures and helpers (only compiled for tests)
#[cfg(test)]
pub mod testing;

// Agent-specific modules
#[cfg(feature = "agent")]
pub mod agent;

// Coordinator-specific modules
#[cfg(feature = "coordinator")]
pub mod aws;
#[cfg(feature = "coordinator")]
pub mod config;
#[cfg(feature = "coordinator")]
pub mod orchestrator;
#[cfg(feature = "coordinator")]
pub mod state;
#[cfg(feature = "coordinator")]
pub mod tui;
#[cfg(feature = "coordinator")]
pub mod wait;
