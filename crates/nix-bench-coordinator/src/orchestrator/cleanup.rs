//! Resource cleanup functions
//!
//! This module handles cleanup of AWS resources (EC2 instances, S3 buckets,
//! IAM roles, security groups) after benchmark runs.

use std::collections::HashMap;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::aws::{classify_anyhow_error, Ec2Client, IamClient, S3Client};
use crate::state::{self, ResourceType, RunStatus};
use crate::tui::{CleanupProgress, InitPhase, TuiMessage};
use super::types::{InstanceState, InstanceStatus};

/// Helper to send cleanup progress to TUI
pub async fn send_cleanup_progress(tx: &Option<mpsc::Sender<TuiMessage>>, progress: CleanupProgress) {
    if let Some(tx) = tx {
        let _ = tx.send(TuiMessage::Phase(InitPhase::CleaningUp(progress))).await;
    }
}

/// Cleanup resources (terminate instances, delete bucket)
/// If `progress_tx` is Some, sends progress updates to the TUI channel
pub async fn cleanup_resources(
    config: &crate::config::RunConfig,
    run_id: &str,
    bucket_name: &str,
    instances: &HashMap<String, InstanceState>,
    progress_tx: Option<mpsc::Sender<TuiMessage>>,
) -> Result<()> {
    let db = state::open_db().await?;
    let ec2 = Ec2Client::new(&config.region).await?;
    let s3 = S3Client::new(&config.region).await?;

    if !config.keep {
        // Get all undeleted resources from the database
        // Note: get_run_resources only returns resources with deleted_at IS NULL,
        // so resources already cleaned up by the termination watcher are excluded
        let db_resources = state::get_run_resources(&db, run_id).await.unwrap_or_default();

        // Collect instance IDs to terminate from both the HashMap and database
        // (database includes instances created but not tracked in HashMap, e.g., if user quit early)
        let mut instance_ids: std::collections::HashSet<String> = instances
            .values()
            .filter(|s| !s.instance_id.is_empty())
            .map(|s| s.instance_id.clone())
            .collect();

        // Add any instances from the database not in the HashMap
        for resource in &db_resources {
            if resource.resource_type == ResourceType::Ec2Instance {
                instance_ids.insert(resource.resource_id.clone());
            }
        }

        // Filter out instances that are already deleted according to DB
        // (the HashMap might have stale data if termination watcher already cleaned them)
        let db_instance_ids: std::collections::HashSet<String> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::Ec2Instance)
            .map(|r| r.resource_id.clone())
            .collect();
        instance_ids.retain(|id| db_instance_ids.contains(id));

        // Count resources for progress tracking
        let iam_roles: Vec<_> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::IamRole && r.deleted_at.is_none())
            .collect();
        let sg_rules: Vec<_> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::SecurityGroupRule && r.deleted_at.is_none())
            .collect();
        let security_groups: Vec<_> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::SecurityGroup && r.deleted_at.is_none())
            .collect();

        let mut progress = CleanupProgress::new(
            instance_ids.len(),
            0, // No EIPs to release
            iam_roles.len(),
            sg_rules.len(),
        );

        // Terminate EC2 instances
        info!(count = instance_ids.len(), "Terminating instances...");
        progress.current_step = format!("Terminating {} EC2 instances...", instance_ids.len());
        send_cleanup_progress(&progress_tx, progress.clone()).await;

        for instance_id in &instance_ids {
            match ec2.terminate_instance(instance_id).await {
                Ok(()) => {
                    info!(instance_id = %instance_id, "Instance terminated");
                    let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, instance_id).await;
                }
                Err(e) => {
                    if classify_anyhow_error(&e).is_not_found() {
                        let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, instance_id).await;
                    } else {
                        warn!(instance_id = %instance_id, error = ?e, "Failed to terminate instance");
                    }
                }
            }
            progress.ec2_instances.0 += 1;
            send_cleanup_progress(&progress_tx, progress.clone()).await;
        }

        // Delete S3 bucket
        info!("Deleting S3 bucket...");
        progress.current_step = "Deleting S3 bucket...".to_string();
        send_cleanup_progress(&progress_tx, progress.clone()).await;

        match s3.delete_bucket(bucket_name).await {
            Ok(()) => {
                let _ = state::mark_resource_deleted(&db, ResourceType::S3Bucket, bucket_name).await;
            }
            Err(e) => {
                if classify_anyhow_error(&e).is_not_found() {
                    let _ = state::mark_resource_deleted(&db, ResourceType::S3Bucket, bucket_name).await;
                } else {
                    warn!(bucket = %bucket_name, error = ?e, "Failed to delete bucket");
                }
            }
        }
        progress.s3_bucket = true;
        send_cleanup_progress(&progress_tx, progress.clone()).await;

        // Delete IAM resources
        if !iam_roles.is_empty() {
            info!(count = iam_roles.len(), "Deleting IAM resources...");
            progress.current_step = format!("Deleting {} IAM roles...", iam_roles.len());
            send_cleanup_progress(&progress_tx, progress.clone()).await;

            let iam = IamClient::new(&config.region).await?;
            for resource in &iam_roles {
                if let Err(e) = iam.delete_benchmark_role(&resource.resource_id).await {
                    warn!(role = %resource.resource_id, error = ?e, "Failed to delete IAM role");
                } else {
                    let _ = state::mark_resource_deleted(
                        &db,
                        ResourceType::IamRole,
                        &resource.resource_id,
                    )
                    .await;
                    // Instance profile has the same name as the role
                    let _ = state::mark_resource_deleted(
                        &db,
                        ResourceType::IamInstanceProfile,
                        &resource.resource_id,
                    )
                    .await;
                }
                progress.iam_roles.0 += 1;
                send_cleanup_progress(&progress_tx, progress.clone()).await;
            }
        }

        // Delete security group rules (for user-provided security groups)
        if !sg_rules.is_empty() {
            info!(count = sg_rules.len(), "Removing security group rules...");
            progress.current_step = format!("Removing {} security group rules...", sg_rules.len());
            send_cleanup_progress(&progress_tx, progress.clone()).await;

                for resource in &sg_rules {
                // Parse resource_id format: "sg-xxx:cidr_ip"
                if let Some((sg_id, cidr_ip)) = resource.resource_id.split_once(':') {
                    match ec2.remove_grpc_ingress_rule(sg_id, cidr_ip).await {
                        Ok(()) => {
                            info!(security_group = %sg_id, cidr_ip = %cidr_ip, "Removed gRPC ingress rule");
                            let _ = state::mark_resource_deleted(&db, ResourceType::SecurityGroupRule, &resource.resource_id).await;
                        }
                        Err(e) => {
                            if classify_anyhow_error(&e).is_not_found() {
                                let _ = state::mark_resource_deleted(&db, ResourceType::SecurityGroupRule, &resource.resource_id).await;
                            } else {
                                warn!(security_group = %sg_id, cidr_ip = %cidr_ip, error = ?e, "Failed to remove security group rule");
                            }
                        }
                    }
                } else {
                    warn!(resource_id = %resource.resource_id, "Invalid SecurityGroupRule resource_id format");
                }
                progress.security_rules.0 += 1;
                send_cleanup_progress(&progress_tx, progress.clone()).await;
            }
        }

        // Delete security groups (for nix-bench-created security groups)
        // Must wait for instances to fully terminate first
        if !security_groups.is_empty() {
            // Wait for all instances to terminate before deleting security groups
            if !instance_ids.is_empty() {
                info!("Waiting for instances to fully terminate before deleting security groups...");
                progress.current_step = "Waiting for instances to terminate...".to_string();
                send_cleanup_progress(&progress_tx, progress.clone()).await;

                for instance_id in &instance_ids {
                    ec2.wait_for_terminated(instance_id).await?;
                }
            }

            info!(count = security_groups.len(), "Deleting security groups...");
            progress.current_step = format!("Deleting {} security groups...", security_groups.len());
            send_cleanup_progress(&progress_tx, progress.clone()).await;

            for resource in &security_groups {
                match ec2.delete_security_group(&resource.resource_id).await {
                    Ok(()) => {
                        info!(sg_id = %resource.resource_id, "Deleted security group");
                        let _ = state::mark_resource_deleted(&db, ResourceType::SecurityGroup, &resource.resource_id).await;
                    }
                    Err(e) => {
                        if classify_anyhow_error(&e).is_not_found() {
                            let _ = state::mark_resource_deleted(&db, ResourceType::SecurityGroup, &resource.resource_id).await;
                        } else {
                            warn!(sg_id = %resource.resource_id, error = ?e, "Failed to delete security group");
                        }
                    }
                }
            }
        }

        // Final progress update
        progress.current_step = "Cleanup complete".to_string();
        send_cleanup_progress(&progress_tx, progress).await;
    } else {
        info!("Keeping instances, bucket, and IAM resources (--keep specified)");
    }

    // Update run status
    let all_complete = !instances.is_empty()
        && instances
            .values()
            .all(|s| s.status == InstanceStatus::Complete);

    state::update_run_status(
        &db,
        run_id,
        if all_complete {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        },
    )
    .await?;

    Ok(())
}
