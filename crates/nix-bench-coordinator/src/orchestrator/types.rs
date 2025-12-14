//! Core types for the orchestrator
//!
//! Contains `InstanceState` and `InstanceStatus` types used to track
//! the state of benchmark instances during a run.

use crate::tui::LogBuffer;

/// Instance state during a benchmark run
#[derive(Debug, Clone)]
pub struct InstanceState {
    /// EC2 instance ID
    pub instance_id: String,
    /// Instance type (e.g., "c6i.xlarge")
    pub instance_type: String,
    /// System architecture (e.g., "x86_64-linux")
    pub system: String,
    /// Current status
    pub status: InstanceStatus,
    /// Number of completed runs
    pub run_progress: u32,
    /// Total number of runs to execute
    pub total_runs: u32,
    /// Build durations in seconds for completed runs
    pub durations: Vec<f64>,
    /// Public IP address (once assigned)
    pub public_ip: Option<String>,
    /// Console/build output buffer (ring buffer capped at 10,000 lines)
    pub console_output: LogBuffer,
}

impl InstanceState {
    /// Create a new instance state with pending status
    pub fn new(instance_type: &str, system: &str, total_runs: u32) -> Self {
        Self {
            instance_id: String::new(),
            instance_type: instance_type.to_string(),
            system: system.to_string(),
            status: InstanceStatus::Pending,
            run_progress: 0,
            total_runs,
            durations: Vec::new(),
            public_ip: None,
            console_output: LogBuffer::new(10_000),
        }
    }

    /// Check if this instance has completed (successfully or with failure)
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, InstanceStatus::Complete | InstanceStatus::Failed)
    }

    /// Check if this instance completed successfully
    pub fn is_success(&self) -> bool {
        matches!(self.status, InstanceStatus::Complete)
    }
}

/// Status of a benchmark instance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstanceStatus {
    /// Instance not yet launched
    #[default]
    Pending,
    /// Instance is launching
    Launching,
    /// Instance is running benchmarks
    Running,
    /// Benchmarks completed successfully
    Complete,
    /// Instance failed
    Failed,
}

impl InstanceStatus {
    /// Get a display string for the status
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Launching => "launching",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for InstanceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_status_display() {
        assert_eq!(InstanceStatus::Pending.to_string(), "pending");
        assert_eq!(InstanceStatus::Running.to_string(), "running");
        assert_eq!(InstanceStatus::Complete.to_string(), "complete");
        assert_eq!(InstanceStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn test_instance_state_new() {
        let state = InstanceState::new("c6i.xlarge", "x86_64-linux", 10);
        assert_eq!(state.instance_type, "c6i.xlarge");
        assert_eq!(state.system, "x86_64-linux");
        assert_eq!(state.total_runs, 10);
        assert_eq!(state.status, InstanceStatus::Pending);
        assert!(state.instance_id.is_empty());
    }

    #[test]
    fn test_instance_state_terminal() {
        let mut state = InstanceState::new("c6i.xlarge", "x86_64-linux", 10);

        assert!(!state.is_terminal());

        state.status = InstanceStatus::Running;
        assert!(!state.is_terminal());

        state.status = InstanceStatus::Complete;
        assert!(state.is_terminal());
        assert!(state.is_success());

        state.status = InstanceStatus::Failed;
        assert!(state.is_terminal());
        assert!(!state.is_success());
    }
}
