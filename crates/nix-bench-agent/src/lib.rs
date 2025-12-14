//! nix-bench-agent - Benchmark agent for EC2 instances
//!
//! This crate provides the agent binary that runs on EC2 instances to execute
//! Nix build benchmarks and report results.
//!
//! # Module Visibility
//!
//! All modules are public to support integration testing from the coordinator crate.
//! For production use, only the binary entry point (`main.rs`) is relevant.

pub mod benchmark;
pub mod bootstrap;
pub mod command;
pub mod config;
pub mod error;
pub mod gc;
pub mod grpc;
pub mod logging;
pub mod results;
