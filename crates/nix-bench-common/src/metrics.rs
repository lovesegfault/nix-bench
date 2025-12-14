//! Shared CloudWatch metrics constants.
//!
//! This module provides the single source of truth for metric dimensions and names.
//! Both the agent (publishing) and coordinator (querying) use these constants
//! to ensure dimension matching.
//!
//! NOTE: The `build_dimensions()` function that creates AWS SDK `Dimension` objects
//! lives in the agent and coordinator crates to avoid pulling AWS SDK dependencies
//! into this lightweight common crate.

/// CloudWatch namespace for all nix-bench metrics
pub const NAMESPACE: &str = "NixBench";

/// Dimension names - used by both agent (publish) and coordinator (query)
pub mod dimensions {
    pub const RUN_ID: &str = "RunId";
    pub const INSTANCE_TYPE: &str = "InstanceType";
    pub const SYSTEM: &str = "System";
}

/// Metric names
pub mod names {
    pub const STATUS: &str = "Status";
    pub const RUN_PROGRESS: &str = "RunProgress";
    pub const RUN_DURATION: &str = "RunDuration";
}

/// Status values for the Status metric
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    /// Agent is initializing
    Pending = 0,
    /// Agent is running benchmarks
    Running = 1,
    /// Agent completed successfully
    Complete = 2,
    /// Agent failed
    Failed = -1,
}

impl Status {
    pub fn as_f64(self) -> f64 {
        self as i32 as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_values() {
        assert_eq!(Status::Pending.as_f64(), 0.0);
        assert_eq!(Status::Running.as_f64(), 1.0);
        assert_eq!(Status::Complete.as_f64(), 2.0);
        assert_eq!(Status::Failed.as_f64(), -1.0);
    }

    #[test]
    fn test_constants() {
        assert_eq!(NAMESPACE, "NixBench");
        assert_eq!(dimensions::RUN_ID, "RunId");
        assert_eq!(dimensions::INSTANCE_TYPE, "InstanceType");
        assert_eq!(dimensions::SYSTEM, "System");
        assert_eq!(names::STATUS, "Status");
        assert_eq!(names::RUN_PROGRESS, "RunProgress");
        assert_eq!(names::RUN_DURATION, "RunDuration");
    }
}
