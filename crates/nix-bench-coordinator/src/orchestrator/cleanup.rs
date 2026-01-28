//! Resource cleanup functions
//!
//! This module handles cleanup of AWS resources (EC2 instances, S3 buckets,
//! IAM roles, security groups) after benchmark runs.
//!
//! Cleanup is handled through a unified queue that processes:
//! - Single instance terminations (when benchmarks complete)
//! - Full cleanup (when user quits or all benchmarks complete)

use std::collections::HashMap;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::types::{InstanceState, InstanceStatus};
use crate::aws::{Ec2Client, IamClient, S3Client, classify_anyhow_error};
use crate::tui::{CleanupProgress, InitPhase, TuiMessage};

/// Request to cleanup resources
#[derive(Debug, Clone)]
pub enum CleanupRequest {
    /// Terminate a single EC2 instance (sent when benchmark completes)
    TerminateInstance {
        instance_type: String,
        instance_id: String,
        public_ip: Option<String>,
    },
    /// Full cleanup - terminate all remaining resources
    /// Sent when user presses 'q' or all benchmarks complete
    FullCleanup {
        region: String,
        keep: bool,
        run_id: String,
        bucket_name: String,
        instances: HashMap<String, InstanceState>,
        /// Security group ID created by nix-bench (if any)
        security_group_id: Option<String>,
        /// IAM role name created by nix-bench (if any)
        iam_role_name: Option<String>,
        /// Security group rules added by nix-bench (sg_id:cidr pairs)
        sg_rules: Vec<String>,
    },
}

/// Helper to send cleanup progress to TUI
async fn send_cleanup_progress(tx: &mpsc::Sender<TuiMessage>, progress: CleanupProgress) {
    let _ = tx
        .send(TuiMessage::Phase(InitPhase::CleaningUp(progress)))
        .await;
}

/// Unified cleanup executor that handles all cleanup requests
///
/// This is the single entry point for all resource cleanup:
/// - Single instance terminations (sent when benchmarks complete)
/// - Full cleanup (sent when user quits)
pub async fn cleanup_executor(
    mut rx: mpsc::Receiver<CleanupRequest>,
    region: String,
    tx: mpsc::Sender<TuiMessage>,
) {
    let ec2 = match Ec2Client::new(&region).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = ?e, "Failed to create EC2 client for cleanup executor");
            return;
        }
    };

    while let Some(request) = rx.recv().await {
        match request {
            CleanupRequest::TerminateInstance {
                instance_type,
                instance_id,
                public_ip,
            } => {
                info!(
                    instance_type = %instance_type,
                    instance_id = %instance_id,
                    "Terminating completed instance"
                );

                match ec2.terminate_instance(&instance_id).await {
                    Ok(()) => {
                        info!(instance_id = %instance_id, "Instance terminated");
                        let _ = tx
                            .send(TuiMessage::InstanceUpdate {
                                instance_type: instance_type.clone(),
                                instance_id: instance_id.clone(),
                                status: InstanceStatus::Terminated,
                                public_ip: public_ip.clone(),
                                run_progress: None,
                                durations: None,
                            })
                            .await;
                    }
                    Err(e) => {
                        if classify_anyhow_error(&e).is_not_found() {
                            debug!(instance_id = %instance_id, "Instance already terminated");
                            let _ = tx
                                .send(TuiMessage::InstanceUpdate {
                                    instance_type: instance_type.clone(),
                                    instance_id: instance_id.clone(),
                                    status: InstanceStatus::Terminated,
                                    public_ip: public_ip.clone(),
                                    run_progress: None,
                                    durations: None,
                                })
                                .await;
                        } else {
                            warn!(instance_id = %instance_id, error = ?e, "Failed to terminate instance");
                        }
                    }
                }
            }

            CleanupRequest::FullCleanup {
                region: cleanup_region,
                keep,
                run_id: _run_id,
                bucket_name,
                instances,
                security_group_id,
                iam_role_name,
                sg_rules,
            } => {
                info!("Starting full cleanup");

                if let Err(e) = do_full_cleanup(
                    &cleanup_region,
                    keep,
                    &bucket_name,
                    &instances,
                    security_group_id.as_deref(),
                    iam_role_name.as_deref(),
                    &sg_rules,
                    &tx,
                )
                .await
                {
                    warn!(error = ?e, "Full cleanup failed");
                }

                debug!("Full cleanup complete, exiting executor");
                return;
            }
        }
    }

    debug!("Cleanup executor channel closed");
}

/// Perform full cleanup of all resources for a run
///
/// Uses in-memory resource tracking (no database) to determine what to clean up.
#[allow(clippy::too_many_arguments)]
async fn do_full_cleanup(
    region: &str,
    keep: bool,
    bucket_name: &str,
    instances: &HashMap<String, InstanceState>,
    security_group_id: Option<&str>,
    iam_role_name: Option<&str>,
    sg_rules: &[String],
    tx: &mpsc::Sender<TuiMessage>,
) -> Result<()> {
    let ec2 = Ec2Client::new(region).await?;
    let s3 = S3Client::new(region).await?;

    if !keep {
        // Collect instance IDs to terminate
        let instance_ids: Vec<String> = instances
            .values()
            .filter(|s| !s.instance_id.is_empty())
            .filter(|s| s.status != InstanceStatus::Terminated)
            .map(|s| s.instance_id.clone())
            .collect();

        let mut progress = CleanupProgress::new(
            instance_ids.len(),
            0, // No EIPs to release
            if iam_role_name.is_some() { 1 } else { 0 },
            sg_rules.len(),
        );

        // Terminate EC2 instances in batch
        info!(count = instance_ids.len(), "Terminating instances...");
        progress.current_step = format!("Terminating {} EC2 instances...", instance_ids.len());
        send_cleanup_progress(tx, progress.clone()).await;

        if !instance_ids.is_empty() {
            if let Err(e) = ec2.terminate_instances(&instance_ids).await {
                warn!(error = ?e, "Failed to terminate instances in batch");
            }
        }
        progress.ec2_instances.0 = instance_ids.len();
        send_cleanup_progress(tx, progress.clone()).await;

        // Delete S3 bucket
        info!("Deleting S3 bucket...");
        progress.current_step = "Deleting S3 bucket...".to_string();
        send_cleanup_progress(tx, progress.clone()).await;

        if let Err(e) = s3.delete_bucket(bucket_name).await {
            warn!(bucket = %bucket_name, error = ?e, "Failed to delete bucket");
        }
        progress.s3_bucket = true;
        send_cleanup_progress(tx, progress.clone()).await;

        // Delete IAM resources
        if let Some(role_name) = iam_role_name {
            info!("Deleting IAM resources...");
            progress.current_step = "Deleting IAM role...".to_string();
            send_cleanup_progress(tx, progress.clone()).await;

            let iam = IamClient::new(region).await?;
            if let Err(e) = iam.delete_benchmark_role(role_name).await {
                warn!(role = %role_name, error = ?e, "Failed to delete IAM role");
            }
            progress.iam_roles.0 += 1;
            send_cleanup_progress(tx, progress.clone()).await;
        }

        // Delete security group rules (for user-provided security groups)
        if !sg_rules.is_empty() {
            info!(count = sg_rules.len(), "Removing security group rules...");
            progress.current_step = format!("Removing {} security group rules...", sg_rules.len());
            send_cleanup_progress(tx, progress.clone()).await;

            for rule in sg_rules {
                if let Some((sg_id, cidr_ip)) = rule.split_once(':') {
                    if let Err(e) = ec2.remove_grpc_ingress_rule(sg_id, cidr_ip).await {
                        warn!(security_group = %sg_id, cidr_ip = %cidr_ip, error = ?e, "Failed to remove security group rule");
                    }
                }
                progress.security_rules.0 += 1;
                send_cleanup_progress(tx, progress.clone()).await;
            }
        }

        // Delete security groups (for nix-bench-created security groups)
        // Must wait for instances to fully terminate first
        if let Some(sg_id) = security_group_id {
            if !instance_ids.is_empty() {
                info!("Waiting for instances to fully terminate before deleting security group...");
                progress.current_step = "Waiting for instances to terminate...".to_string();
                send_cleanup_progress(tx, progress.clone()).await;

                ec2.wait_for_all_terminated(&instance_ids).await?;
            }

            info!("Deleting security group...");
            progress.current_step = "Deleting security group...".to_string();
            send_cleanup_progress(tx, progress.clone()).await;

            if let Err(e) = ec2.delete_security_group(sg_id).await {
                warn!(sg_id = %sg_id, error = ?e, "Failed to delete security group");
            }
        }

        // Final progress update
        progress.current_step = "Cleanup complete".to_string();
        send_cleanup_progress(tx, progress).await;
    } else {
        info!("Keeping instances, bucket, and IAM resources (--keep specified)");
    }

    Ok(())
}

/// Direct cleanup function for non-TUI mode
///
/// This is a simpler entry point that doesn't require setting up channels.
/// Progress updates are logged instead of sent to a TUI.
pub async fn cleanup_resources_no_tui(
    config: &crate::config::RunConfig,
    _run_id: &str,
    bucket_name: &str,
    instances: &HashMap<String, InstanceState>,
    security_group_id: Option<&str>,
    iam_role_name: Option<&str>,
    sg_rules: &[String],
) -> Result<()> {
    // Create a dummy channel that we won't actually use
    let (tx, _rx) = mpsc::channel::<TuiMessage>(1);

    do_full_cleanup(
        &config.region,
        config.keep,
        bucket_name,
        instances,
        security_group_id,
        iam_role_name,
        sg_rules,
        &tx,
    )
    .await
}
