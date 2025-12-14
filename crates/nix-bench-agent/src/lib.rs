//! nix-bench-agent - Benchmark agent for EC2 instances
//!
//! This crate provides the agent binary that runs on EC2 instances to execute
//! Nix build benchmarks and report results.

pub mod benchmark;
pub mod bootstrap;
pub mod config;
pub mod error;
pub mod grpc;
pub mod logging;
pub mod results;
