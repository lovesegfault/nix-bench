//! Instance monitoring functions
//!
//! This module handles monitoring of EC2 instances during benchmark runs,
//! including bootstrap failure detection and automatic termination of
//! completed instances.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::aws::Ec2Client;
use crate::state::{self, ResourceType};
use crate::tui::TuiMessage;
use super::types::{InstanceState, InstanceStatus};
use super::user_data::detect_bootstrap_failure;

/// Poll EC2 console output for bootstrap failure detection
/// This monitors instances during the bootstrap phase before gRPC is available
pub async fn poll_bootstrap_status(
    instances: HashMap<String, InstanceState>,
    tx: mpsc::Sender<TuiMessage>,
    region: String,
    timeout_secs: u64,
    start_time: Instant,
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

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Request to terminate an instance
#[derive(Debug, Clone)]
pub struct TerminationRequest {
    pub instance_type: String,
    pub instance_id: String,
    pub public_ip: Option<String>,
}

/// Execute termination requests as they come in from the TUI
///
/// This replaces the old polling-based watcher. The TUI now detects Complete status
/// via its own gRPC poll and sends termination requests through the channel.
/// This eliminates redundant polling and ensures faster termination.
pub async fn termination_executor(
    mut rx: mpsc::Receiver<TerminationRequest>,
    region: String,
    tx: mpsc::Sender<TuiMessage>,
) {
    let ec2 = match Ec2Client::new(&region).await {
        Ok(c) => c,
        Err(e) => {
            error!(error = ?e, "Failed to create EC2 client for termination executor");
            return;
        }
    };

    let db = match state::open_db().await {
        Ok(d) => d,
        Err(e) => {
            error!(error = ?e, "Failed to get database for termination executor");
            return;
        }
    };

    while let Some(request) = rx.recv().await {
        info!(
            instance_type = %request.instance_type,
            instance_id = %request.instance_id,
            "Received termination request"
        );

        match ec2.terminate_instance(&request.instance_id).await {
            Ok(()) => {
                info!(
                    instance_id = %request.instance_id,
                    instance_type = %request.instance_type,
                    "Instance terminated"
                );
                let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, &request.instance_id).await;

                // Notify TUI that instance was terminated
                let _ = tx
                    .send(TuiMessage::InstanceUpdate {
                        instance_type: request.instance_type.clone(),
                        instance_id: request.instance_id.clone(),
                        status: InstanceStatus::Terminated,
                        public_ip: request.public_ip.clone(),
                        run_progress: None,
                        durations: None,
                    })
                    .await;
            }
            Err(e) => {
                let error_str = format!("{:?}", e);
                if error_str.contains("InvalidInstanceID.NotFound") {
                    // Already terminated
                    debug!(
                        instance_id = %request.instance_id,
                        "Instance already terminated"
                    );
                    let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, &request.instance_id).await;

                    let _ = tx
                        .send(TuiMessage::InstanceUpdate {
                            instance_type: request.instance_type.clone(),
                            instance_id: request.instance_id.clone(),
                            status: InstanceStatus::Terminated,
                            public_ip: request.public_ip.clone(),
                            run_progress: None,
                            durations: None,
                        })
                        .await;
                } else {
                    warn!(
                        instance_id = %request.instance_id,
                        error = ?e,
                        "Failed to terminate instance"
                    );
                }
            }
        }
    }

    debug!("Termination executor channel closed, exiting");
}
