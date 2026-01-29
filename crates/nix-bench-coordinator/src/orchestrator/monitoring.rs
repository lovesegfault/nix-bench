//! Instance monitoring functions
//!
//! This module handles monitoring of EC2 instances during benchmark runs,
//! specifically bootstrap failure detection via EC2 console output.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, warn};

use super::types::{InstanceState, InstanceStatus};
use super::user_data::detect_bootstrap_failure;
use crate::aws::{Ec2Client, FromAwsContext};
use crate::tui::TuiMessage;

/// Poll EC2 console output for bootstrap failure detection
/// This monitors instances during the bootstrap phase before gRPC is available.
/// Exits when all instances have been checked and failed, timeout is reached,
/// or the cancellation token is triggered.
pub async fn poll_bootstrap_status(
    instances: HashMap<String, InstanceState>,
    tx: mpsc::Sender<TuiMessage>,
    region: String,
    timeout_secs: u64,
    start_time: Instant,
    cancel: CancellationToken,
) {
    let ec2 = match Ec2Client::new(&region).await {
        Ok(c) => c,
        Err(_) => return,
    };

    // Track which instances have already been marked as failed
    let mut failed_instances: HashSet<String> = HashSet::new();

    // Poll interval for EC2 console output
    const POLL_INTERVAL: Duration = Duration::from_secs(10);

    loop {
        // Check for timeout
        let elapsed = start_time.elapsed().as_secs();
        if timeout_secs > 0 && elapsed > timeout_secs {
            warn!(
                elapsed_secs = elapsed,
                timeout_secs = timeout_secs,
                "Run timeout exceeded"
            );
            for (instance_type, state) in &instances {
                if !failed_instances.contains(instance_type) {
                    error!(instance_type = %instance_type, "Instance timed out");
                    let _ = tx
                        .send(TuiMessage::InstanceUpdate {
                            instance_type: instance_type.clone(),
                            instance_id: state.instance_id.clone(),
                            status: InstanceStatus::Failed,
                            public_ip: state.public_ip.clone(),
                            run_progress: None,
                            durations: None,
                        })
                        .await;
                }
            }
            break;
        }

        for (instance_type, state) in &instances {
            // Skip already failed instances
            if failed_instances.contains(instance_type) {
                continue;
            }

            // Check EC2 console output for bootstrap failures
            if !state.instance_id.is_empty() {
                if let Ok(Some(console_output)) = ec2.get_console_output(&state.instance_id).await {
                    // Check for bootstrap failures
                    if let Some(failure_pattern) = detect_bootstrap_failure(&console_output) {
                        error!(
                            instance_type = %instance_type,
                            instance_id = %state.instance_id,
                            pattern = %failure_pattern,
                            "Bootstrap failure detected"
                        );
                        failed_instances.insert(instance_type.clone());
                        let _ = tx
                            .send(TuiMessage::InstanceUpdate {
                                instance_type: instance_type.clone(),
                                instance_id: state.instance_id.clone(),
                                status: InstanceStatus::Failed,
                                public_ip: state.public_ip.clone(),
                                run_progress: None,
                                durations: None,
                            })
                            .await;
                        // Send console output so user can see what happened
                        let _ = tx
                            .send(TuiMessage::ConsoleOutput {
                                instance_type: instance_type.clone(),
                                output: console_output,
                            })
                            .await;
                    }
                }
            }
        }

        // Wait with cancellation support
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
            _ = cancel.cancelled() => {
                break;
            }
        }
    }
}
