//! Instance monitoring functions
//!
//! This module handles monitoring of EC2 instances during benchmark runs,
//! including bootstrap failure detection and automatic termination of
//! completed instances.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::aws::{Ec2Client, GrpcStatusPoller};
use crate::state::{self, ResourceType};
use crate::tui::TuiMessage;
use super::types::{InstanceState, InstanceStatus};
use super::user_data::detect_bootstrap_failure;
use super::GRPC_PORT;

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

/// Watch for completed instances and terminate them immediately
///
/// This task polls gRPC status and terminates instances as soon as they report complete,
/// rather than waiting for all instances to finish before cleanup.
pub async fn watch_and_terminate_completed(
    instances: HashMap<String, InstanceState>,
    region: String,
    _run_id: String,
    tls_config: nix_bench_common::TlsConfig,
    tx: mpsc::Sender<TuiMessage>,
) {
    use nix_bench_common::StatusCode;

    let ec2 = match Ec2Client::new(&region).await {
        Ok(c) => c,
        Err(e) => {
            error!(error = ?e, "Failed to create EC2 client for termination watcher");
            return;
        }
    };

    let db = match state::open_db().await {
        Ok(d) => d,
        Err(e) => {
            error!(error = ?e, "Failed to get database for termination watcher");
            return;
        }
    };

    // Build list of instances with IPs for polling
    let instances_with_ips: Vec<(String, String)> = instances
        .iter()
        .filter_map(|(instance_type, state)| {
            state
                .public_ip
                .as_ref()
                .map(|ip| (instance_type.clone(), ip.clone()))
        })
        .collect();

    if instances_with_ips.is_empty() {
        debug!("No instances with IPs for termination watcher");
        return;
    }

    // Track which instances have been terminated
    let mut terminated: HashSet<String> = HashSet::new();

    // Poll interval for status checks
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    loop {
        // Check if all instances are done
        if terminated.len() >= instances.len() {
            debug!("All instances terminated, exiting termination watcher");
            break;
        }

        // Poll status from all instances with mTLS
        let poller = GrpcStatusPoller::new(&instances_with_ips, GRPC_PORT, tls_config.clone());

        let status_map = poller.poll_status().await;

        for (instance_type, status) in &status_map {
            // Skip already terminated instances
            if terminated.contains(instance_type) {
                continue;
            }

            // Check if instance is complete
            if let Some(status_code) = status.status {
                if status_code == StatusCode::Complete {
                    info!(instance_type = %instance_type, "Instance complete, terminating immediately");

                    // Get instance ID
                    if let Some(state) = instances.get(instance_type) {
                        let instance_id = &state.instance_id;

                        // Send final status update with durations BEFORE terminating
                        // This ensures the TUI receives the complete status even if gRPC polling
                        // fails after termination
                        let _ = tx
                            .send(TuiMessage::InstanceUpdate {
                                instance_type: instance_type.clone(),
                                instance_id: state.instance_id.clone(),
                                status: InstanceStatus::Complete,
                                public_ip: state.public_ip.clone(),
                                run_progress: status.run_progress,
                                durations: Some(status.durations.clone()),
                            })
                            .await;

                        // Terminate the instance
                        match ec2.terminate_instance(instance_id).await {
                            Ok(()) => {
                                info!(instance_id = %instance_id, instance_type = %instance_type, "Instance terminated");
                                let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, instance_id).await;
                                terminated.insert(instance_type.clone());

                                // Notify TUI that instance was terminated
                                let _ = tx
                                    .send(TuiMessage::InstanceUpdate {
                                        instance_type: instance_type.clone(),
                                        instance_id: state.instance_id.clone(),
                                        status: InstanceStatus::Terminated,
                                        public_ip: state.public_ip.clone(),
                                        run_progress: None,
                                        durations: None,
                                    })
                                    .await;
                            }
                            Err(e) => {
                                let error_str = format!("{:?}", e);
                                if error_str.contains("InvalidInstanceID.NotFound") {
                                    // Already terminated
                                    let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, instance_id).await;
                                    terminated.insert(instance_type.clone());

                                    // Notify TUI that instance was terminated
                                    let _ = tx
                                        .send(TuiMessage::InstanceUpdate {
                                            instance_type: instance_type.clone(),
                                            instance_id: state.instance_id.clone(),
                                            status: InstanceStatus::Terminated,
                                            public_ip: state.public_ip.clone(),
                                            run_progress: None,
                                            durations: None,
                                        })
                                        .await;
                                } else {
                                    warn!(instance_id = %instance_id, error = ?e, "Failed to terminate instance");
                                    // Don't add to terminated set - watcher will retry
                                }
                            }
                        }
                    }
                }
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
