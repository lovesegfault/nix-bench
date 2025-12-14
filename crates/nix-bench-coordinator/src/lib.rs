//! nix-bench-coordinator - TUI-based EC2 orchestrator
//!
//! This crate provides the coordinator binary that manages EC2 instances
//! and displays real-time benchmark progress via a TUI.

pub mod aws;
pub mod config;
pub mod orchestrator;
pub mod state;
pub mod tui;
pub mod wait;

// Re-export build_dimensions for coordinator use
pub use aws::build_dimensions;
