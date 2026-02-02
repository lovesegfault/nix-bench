//! Orchestration events emitted by `RunEngine`
//!
//! These events are the universal interface between the orchestration engine
//! and its consumers (TUI or CLI). Both modes receive the same events and
//! decide how to present them.

/// Event emitted by `RunEngine` during a benchmark run
#[derive(Debug, Clone)]
pub enum RunEvent {
    /// Instance status changed (check `engine.instances()` for new state)
    InstanceUpdated { instance_type: String },
    /// Instance reached terminal state and will be auto-terminated
    InstanceTerminal { instance_type: String },
    /// Instance has been terminated
    InstanceTerminated { instance_type: String },
    /// All instances are complete and results captured
    AllComplete,
    /// Timeout exceeded for an instance
    Timeout { instance_type: String },
    /// Bootstrap failure detected via EC2 console output
    BootstrapFailure {
        instance_type: String,
        pattern: String,
    },
}
