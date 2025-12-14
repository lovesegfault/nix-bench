//! Shared run result type for benchmark results
//!
//! Used by both agent and coordinator for tracking individual run outcomes.

use serde::{Deserialize, Serialize};

/// Result of a single benchmark run
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunResult {
    /// Run number (1-based slot number)
    pub run_number: u32,
    /// Duration in seconds (0.0 if failed before timing)
    pub duration_secs: f64,
    /// Whether the run completed successfully
    pub success: bool,
}

impl RunResult {
    /// Create a successful run result
    pub fn success(run_number: u32, duration_secs: f64) -> Self {
        Self {
            run_number,
            duration_secs,
            success: true,
        }
    }

    /// Create a failed run result
    pub fn failure(run_number: u32) -> Self {
        Self {
            run_number,
            duration_secs: 0.0,
            success: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_success_constructor() {
        let result = RunResult::success(1, 45.5);
        assert_eq!(result.run_number, 1);
        assert_eq!(result.duration_secs, 45.5);
        assert!(result.success);
    }

    #[test]
    fn test_failure_constructor() {
        let result = RunResult::failure(3);
        assert_eq!(result.run_number, 3);
        assert_eq!(result.duration_secs, 0.0);
        assert!(!result.success);
    }

    #[test]
    fn test_serialization() {
        let result = RunResult::success(1, 45.678);
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"run_number\":1"));
        assert!(json.contains("\"duration_secs\":45.678"));
        assert!(json.contains("\"success\":true"));

        let parsed: RunResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, result);
    }
}
