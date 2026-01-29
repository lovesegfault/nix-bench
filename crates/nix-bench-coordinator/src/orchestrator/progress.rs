//! Progress reporting abstractions for the orchestrator
//!
//! Provides traits and implementations for reporting initialization
//! progress to different outputs (TUI channel, stdout logging).

use super::types::InstanceStatus;
use crate::tui::{CleanupProgress, InitPhase, TuiMessage};
use tokio::sync::mpsc;
use tracing::info;

/// Generic progress reporter for sending typed updates to either
/// a TUI channel or stdout logs.
pub trait ProgressReporter<T>: Send + Sync {
    fn report(&self, update: &T);
}

/// Instance update information for progress reporting
#[derive(Debug, Clone)]
pub struct InstanceUpdate {
    pub instance_type: String,
    pub instance_id: String,
    pub status: InstanceStatus,
    pub public_ip: Option<String>,
}

/// Trait for reporting initialization progress
///
/// This trait abstracts the progress reporting mechanism, allowing
/// the same initialization logic to work in both TUI and non-TUI modes.
pub trait InitProgressReporter: Send + Sync {
    /// Report a phase change
    fn report_phase(&self, phase: InitPhase);

    /// Report AWS account info
    fn report_account_info(&self, account_id: &str);

    /// Report run info (run ID and bucket name)
    fn report_run_info(&self, run_id: &str, bucket_name: &str);

    /// Report an instance state update
    fn report_instance_update(&self, update: InstanceUpdate);

    /// Report console output for an instance (for error messages during init)
    fn report_console_output(&self, instance_type: &str, output: String);

    /// Check if the operation should be cancelled
    fn is_cancelled(&self) -> bool;
}

/// Progress reporter that sends messages to a TUI channel
pub struct ChannelReporter {
    tx: mpsc::Sender<TuiMessage>,
    cancel: tokio_util::sync::CancellationToken,
}

impl ChannelReporter {
    /// Create a new channel reporter
    pub fn new(tx: mpsc::Sender<TuiMessage>, cancel: tokio_util::sync::CancellationToken) -> Self {
        Self { tx, cancel }
    }

    /// Send a message, ignoring errors (TUI may be closed)
    fn send(&self, msg: TuiMessage) {
        let _ = self.tx.try_send(msg);
    }
}

impl InitProgressReporter for ChannelReporter {
    fn report_phase(&self, phase: InitPhase) {
        self.send(TuiMessage::Phase(phase));
    }

    fn report_account_info(&self, account_id: &str) {
        self.send(TuiMessage::AccountInfo {
            account_id: account_id.to_string(),
        });
    }

    fn report_run_info(&self, run_id: &str, bucket_name: &str) {
        self.send(TuiMessage::RunInfo {
            run_id: run_id.to_string(),
            bucket_name: bucket_name.to_string(),
        });
    }

    fn report_instance_update(&self, update: InstanceUpdate) {
        self.send(TuiMessage::InstanceUpdate {
            instance_type: update.instance_type,
            instance_id: update.instance_id,
            status: update.status,
            public_ip: update.public_ip,
            run_progress: None,
            durations: None,
        });
    }

    fn report_console_output(&self, instance_type: &str, output: String) {
        self.send(TuiMessage::ConsoleOutput {
            instance_type: instance_type.to_string(),
            output,
        });
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

impl ProgressReporter<CleanupProgress> for ChannelReporter {
    fn report(&self, progress: &CleanupProgress) {
        self.send(TuiMessage::Phase(InitPhase::CleaningUp(progress.clone())));
    }
}

/// Progress reporter that logs to stdout (for non-TUI mode)
pub struct LogReporter;

impl LogReporter {
    /// Create a new log reporter
    pub fn new() -> Self {
        Self
    }
}

impl Default for LogReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl InitProgressReporter for LogReporter {
    fn report_phase(&self, phase: InitPhase) {
        info!(phase = ?phase, "Initialization phase");
    }

    fn report_account_info(&self, account_id: &str) {
        info!(account_id = %account_id, "AWS account validated");
    }

    fn report_run_info(&self, run_id: &str, bucket_name: &str) {
        info!(run_id = %run_id, bucket_name = %bucket_name, "Run initialized");
    }

    fn report_instance_update(&self, update: InstanceUpdate) {
        info!(
            instance_type = %update.instance_type,
            instance_id = %update.instance_id,
            status = %update.status,
            public_ip = ?update.public_ip,
            "Instance update"
        );
    }

    fn report_console_output(&self, instance_type: &str, output: String) {
        info!(instance_type = %instance_type, "Console output:\n{}", output);
    }

    fn is_cancelled(&self) -> bool {
        // Non-interactive mode doesn't support cancellation during init
        false
    }
}

impl ProgressReporter<CleanupProgress> for LogReporter {
    fn report(&self, progress: &CleanupProgress) {
        info!(step = %progress.current_step, "Cleanup progress");
    }
}
