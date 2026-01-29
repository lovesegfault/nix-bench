//! Core types for the orchestrator
//!
//! Contains `InstanceState` and `InstanceStatus` types used to track
//! the state of benchmark instances during a run.

use std::collections::HashMap;

use crate::log_buffer::LogBuffer;
use nix_bench_common::{Architecture, RunResult, StatusCode};

/// Instance state during a benchmark run
#[derive(Debug, Clone)]
pub struct InstanceState {
    /// EC2 instance ID
    pub instance_id: String,
    /// Instance type (e.g., "c6i.xlarge")
    pub instance_type: String,
    /// System architecture
    pub system: Architecture,
    /// Current status
    pub status: InstanceStatus,
    /// Number of completed runs
    pub run_progress: u32,
    /// Total number of runs to execute
    pub total_runs: u32,
    /// Detailed run results with success/failure status
    pub run_results: Vec<RunResult>,
    /// Public IP address (once assigned)
    pub public_ip: Option<String>,
    /// Console/build output buffer (ring buffer capped at 10,000 lines)
    pub console_output: LogBuffer,
}

impl InstanceState {
    /// Create a new instance state with pending status
    pub fn new(instance_type: &str, system: Architecture, total_runs: u32) -> Self {
        Self {
            instance_id: String::new(),
            instance_type: instance_type.to_string(),
            system,
            status: InstanceStatus::Pending,
            run_progress: 0,
            total_runs,
            run_results: Vec::new(),
            public_ip: None,
            console_output: LogBuffer::new(10_000),
        }
    }

    /// Get successful durations from run results
    pub fn durations(&self) -> Vec<f64> {
        self.run_results
            .iter()
            .filter(|r| r.success)
            .map(|r| r.duration_secs)
            .collect()
    }

    /// Check if this instance has completed (successfully or with failure)
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            InstanceStatus::Complete | InstanceStatus::Failed
        )
    }

    /// Check if this instance completed successfully
    pub fn is_success(&self) -> bool {
        matches!(self.status, InstanceStatus::Complete)
    }
}

/// Collect instance types paired with their public IPs.
///
/// Filters to only instances that have a public IP assigned.
pub fn instances_with_ips(instances: &HashMap<String, InstanceState>) -> Vec<(String, String)> {
    instances
        .iter()
        .filter_map(|(instance_type, state)| {
            state
                .public_ip
                .as_ref()
                .map(|ip| (instance_type.clone(), ip.clone()))
        })
        .collect()
}

/// Status of a benchmark instance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, strum::Display, strum::AsRefStr)]
#[strum(serialize_all = "lowercase")]
pub enum InstanceStatus {
    /// Instance not yet launched
    #[default]
    Pending,
    /// Instance is launching (EC2 launch API called)
    Launching,
    /// EC2 is running, waiting for agent to respond
    Starting,
    /// Instance is running benchmarks (agent responding via gRPC)
    Running,
    /// Benchmarks completed successfully
    Complete,
    /// Instance failed
    Failed,
    /// Instance has been terminated
    Terminated,
}

impl InstanceStatus {
    /// Map a gRPC `StatusCode` to an `InstanceStatus`.
    pub fn from_status_code(code: StatusCode) -> Self {
        match code {
            StatusCode::Complete => Self::Complete,
            StatusCode::Failed => Self::Failed,
            StatusCode::Running | StatusCode::Bootstrap | StatusCode::Warmup => Self::Running,
            StatusCode::Pending => Self::Pending,
        }
    }
}
