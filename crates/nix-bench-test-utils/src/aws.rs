//! AWS test utilities
//!
//! Provides region detection and unique run ID generation for AWS integration tests.

use chrono::Utc;

/// Get the AWS region for tests.
///
/// Checks environment variables in order:
/// 1. AWS_REGION
/// 2. AWS_DEFAULT_REGION
/// 3. Falls back to us-east-2
///
/// # Example
///
/// ```
/// use nix_bench_test_utils::aws::get_test_region;
///
/// let region = get_test_region();
/// // Returns "us-east-2" if no env vars are set
/// ```
pub fn get_test_region() -> String {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-2".to_string())
}

/// Generate a unique run ID for test resources.
///
/// Format: `test-{timestamp_ms}-{random}` using milliseconds and random suffix.
/// This ensures unique resource names even when tests start simultaneously.
///
/// # Example
///
/// ```
/// use nix_bench_test_utils::aws::test_run_id;
///
/// let run_id = test_run_id();
/// assert!(run_id.starts_with("test-"));
/// ```
pub fn test_run_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    let ts = Utc::now().timestamp_millis();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("test-{}-{}", ts, counter)
}

/// Generate a unique bucket name for test resources.
///
/// Format: `nix-bench-test-{timestamp}`
///
/// # Example
///
/// ```
/// use nix_bench_test_utils::aws::test_bucket_name;
///
/// let bucket = test_bucket_name();
/// assert!(bucket.starts_with("nix-bench-test-"));
/// ```
pub fn test_bucket_name() -> String {
    format!("nix-bench-{}", test_run_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_test_region_default() {
        // Temporarily clear env vars to test default
        let original_region = std::env::var("AWS_REGION").ok();
        let original_default = std::env::var("AWS_DEFAULT_REGION").ok();

        // SAFETY: Test-only code, tests are run serially with --test-threads=1
        // or env mutation is acceptable for this test.
        unsafe {
            std::env::remove_var("AWS_REGION");
            std::env::remove_var("AWS_DEFAULT_REGION");
        }

        let region = get_test_region();
        assert_eq!(region, "us-east-2");

        // Restore env vars
        unsafe {
            if let Some(r) = original_region {
                std::env::set_var("AWS_REGION", r);
            }
            if let Some(r) = original_default {
                std::env::set_var("AWS_DEFAULT_REGION", r);
            }
        }
    }

    #[test]
    fn test_run_id_format() {
        let run_id = test_run_id();
        assert!(run_id.starts_with("test-"));
        // Format: test-{timestamp_ms}-{counter}
        let parts: Vec<&str> = run_id.strip_prefix("test-").unwrap().split('-').collect();
        assert_eq!(parts.len(), 2);
        parts[0].parse::<i64>().expect("Should be valid timestamp");
        parts[1].parse::<u32>().expect("Should be valid counter");
    }

    #[test]
    fn test_run_id_unique() {
        let id1 = test_run_id();
        let id2 = test_run_id();
        let id3 = test_run_id();
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
    }

    #[test]
    fn test_bucket_name_format() {
        let bucket = test_bucket_name();
        assert!(bucket.starts_with("nix-bench-test-"));
    }
}
