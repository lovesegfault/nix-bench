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
//! The `metrics` module contains shared constants for CloudWatch metrics,
//! ensuring dimension consistency between the agent and coordinator.

pub mod metrics;

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
