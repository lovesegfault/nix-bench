//! Shared utilities for AWS integration tests
//!
//! Provides region detection and unique run IDs.

use chrono::Utc;

/// Get the AWS region for tests.
///
/// Checks environment variables in order:
/// 1. AWS_REGION
/// 2. AWS_DEFAULT_REGION
/// 3. Falls back to us-east-2
pub fn get_test_region() -> String {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-2".to_string())
}

/// Generate a unique run ID for test resources.
///
/// Format: `test-{timestamp}` where timestamp is Unix seconds.
/// This ensures unique resource names across test runs.
pub fn test_run_id() -> String {
    format!("test-{}", Utc::now().timestamp())
}
