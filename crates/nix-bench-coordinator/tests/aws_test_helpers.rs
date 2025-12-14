//! Shared utilities for AWS integration tests
//!
//! Re-exports from nix-bench-test-utils for backwards compatibility.

pub use nix_bench_test_utils::aws::{get_test_region, test_run_id};
// Also available: nix_bench_test_utils::aws::test_bucket_name
