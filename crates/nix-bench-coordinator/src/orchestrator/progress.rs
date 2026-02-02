//! Progress reporting for the orchestrator

use super::types::InstanceStatus;
use crate::tui::{CleanupProgress, InitPhase, TuiMessage};
use tokio::sync::mpsc;
use tracing::info;

/// Instance update information for progress reporting
#[derive(Debug, Clone)]
pub struct InstanceUpdate {
    pub instance_type: String,
    pub instance_id: String,
    pub status: InstanceStatus,
    pub public_ip: Option<String>,
}

/// Unified progress reporter for TUI and non-TUI modes
pub enum Reporter {
    Channel { tx: mpsc::Sender<TuiMessage> },
    Log,
}

impl Reporter {
    pub fn channel(tx: mpsc::Sender<TuiMessage>) -> Self {
        Self::Channel { tx }
    }

    fn send(&self, msg: TuiMessage) {
        if let Reporter::Channel { tx } = self {
            let _ = tx.try_send(msg);
        }
    }

    pub fn report_phase(&self, phase: InitPhase) {
        match self {
            Reporter::Channel { .. } => self.send(TuiMessage::Phase(phase)),
            Reporter::Log => info!(phase = ?phase, "Initialization phase"),
        }
    }

    pub fn report_account_info(&self, account_id: &str) {
        match self {
            Reporter::Channel { .. } => {
                self.send(TuiMessage::AccountInfo {
                    account_id: account_id.to_string(),
                });
            }
            Reporter::Log => info!(account_id = %account_id, "AWS account validated"),
        }
    }

    pub fn report_instance_update(&self, update: InstanceUpdate) {
        match self {
            Reporter::Channel { .. } => {
                self.send(TuiMessage::InstanceUpdate {
                    instance_type: update.instance_type,
                    instance_id: update.instance_id,
                    status: update.status,
                    public_ip: update.public_ip,
                    run_progress: None,
                });
            }
            Reporter::Log => {
                info!(
                    instance_type = %update.instance_type,
                    instance_id = %update.instance_id,
                    status = %update.status,
                    public_ip = ?update.public_ip,
                    "Instance update"
                );
            }
        }
    }

    pub fn report_console_output(&self, instance_type: &str, output: String) {
        match self {
            Reporter::Channel { .. } => {
                self.send(TuiMessage::ConsoleOutput {
                    instance_type: instance_type.to_string(),
                    output,
                });
            }
            Reporter::Log => info!(instance_type = %instance_type, "Console output:\n{}", output),
        }
    }

    pub fn report_cleanup(&self, progress: &CleanupProgress) {
        match self {
            Reporter::Channel { .. } => {
                self.send(TuiMessage::Phase(InitPhase::CleaningUp(progress.clone())));
            }
            Reporter::Log => info!(step = %progress.current_step, "Cleanup progress"),
        }
    }
}
