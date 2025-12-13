//! Shared CloudWatch metrics constants and types.
//!
//! This module is the single source of truth for metric dimensions and names.
//! Both the agent (publishing) and coordinator (querying) use these constants
//! to ensure dimension matching.

use aws_sdk_cloudwatch::types::Dimension;

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

/// Build the dimension vector for CloudWatch metrics.
///
/// This function is used by both:
/// - Agent: when publishing metrics with `PutMetricData`
/// - Coordinator: when querying metrics with `GetMetricStatistics`
///
/// Using this shared function guarantees dimension consistency.
pub fn build_dimensions(run_id: &str, instance_type: &str, system: &str) -> Vec<Dimension> {
    vec![
        Dimension::builder()
            .name(dimensions::RUN_ID)
            .value(run_id)
            .build(),
        Dimension::builder()
            .name(dimensions::INSTANCE_TYPE)
            .value(instance_type)
            .build(),
        Dimension::builder()
            .name(dimensions::SYSTEM)
            .value(system)
            .build(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dimensions_are_consistent() {
        let dims = build_dimensions("run-123", "c7i.metal", "x86_64-linux");
        assert_eq!(dims.len(), 3);

        // Verify all dimension names are present
        let names: Vec<_> = dims.iter().filter_map(|d| d.name()).collect();
        assert!(names.contains(&dimensions::RUN_ID));
        assert!(names.contains(&dimensions::INSTANCE_TYPE));
        assert!(names.contains(&dimensions::SYSTEM));
    }

    #[test]
    fn test_status_values() {
        assert_eq!(Status::Pending.as_f64(), 0.0);
        assert_eq!(Status::Running.as_f64(), 1.0);
        assert_eq!(Status::Complete.as_f64(), 2.0);
        assert_eq!(Status::Failed.as_f64(), -1.0);
    }
}
