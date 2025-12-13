//! TUI dashboard for benchmark monitoring

mod app;
mod ui;
pub mod widgets;

pub use app::{App, InitPhase};

/// Message sent to TUI to update state
#[derive(Debug, Clone)]
pub enum TuiMessage {
    /// Update init phase
    Phase(InitPhase),
    /// Set run ID and bucket name
    RunInfo { run_id: String, bucket_name: String },
    /// Update instance state
    InstanceUpdate {
        instance_type: String,
        instance_id: String,
        status: crate::orchestrator::InstanceStatus,
        public_ip: Option<String>,
    },
    /// Update console output for an instance
    ConsoleOutput {
        instance_type: String,
        output: String,
    },
}
