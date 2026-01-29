//! nix-bench-agent - Benchmark agent for EC2 instances
//!
//! This crate provides the agent binary that runs on EC2 instances to execute
//! Nix build benchmarks and report results.
//!
//! # Module Visibility
//!
//! Only the public gRPC interface and error types are exposed. Internal modules
//! are `pub(crate)` to encourage testing through the gRPC interface rather than
//! reaching into implementation details.

pub mod benchmark;
pub mod bootstrap;
pub(crate) mod command;
pub mod config;
pub mod gc;
pub mod grpc;
pub mod logging;
pub mod results;
