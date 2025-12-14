//! Shared test utilities for nix-bench
//!
//! This crate provides common test helpers that can be used across
//! multiple test modules without circular dependencies.
//!
//! ## Modules
//!
//! - [`aws`]: AWS region detection and test run ID generation
//! - [`db`]: In-memory SQLite database setup for testing
//! - [`tls`]: TLS certificate generation and crypto initialization

pub mod aws;
pub mod db;
pub mod tls;

// Re-export commonly used items
pub use aws::{get_test_region, test_run_id};
pub use db::open_test_db;
pub use tls::init_crypto;
