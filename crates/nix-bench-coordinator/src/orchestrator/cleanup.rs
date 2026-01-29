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

use super::progress::Reporter;
use super::types::{InstanceState, InstanceStatus};
use crate::aws::context::AwsContext;
use crate::aws::{Ec2Client, IamClient, S3Client, classify_anyhow_error, send_ack_complete};
use crate::tui::{CleanupProgress, TuiMessage};
use nix_bench_common::RunId;
use nix_bench_common::defaults::DEFAULT_GRPC_PORT;

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
        run_id: RunId,
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

/// Unified cleanup executor that handles all cleanup requests
///
/// This is the single entry point for all resource cleanup:
/// - Single instance terminations (sent when benchmarks complete)
/// - Full cleanup (sent when user quits)
///
/// The optional `tls_config` is used to send acknowledgment RPCs to agents
/// before terminating their instances for a clean shutdown.
pub async fn cleanup_executor(
    mut rx: mpsc::Receiver<CleanupRequest>,
    region: String,
    tx: mpsc::Sender<TuiMessage>,
    run_id: RunId,
    tls_config: Option<nix_bench_common::TlsConfig>,
) {
    let ctx = AwsContext::new(&region).await;
    let ec2 = Ec2Client::from_context(&ctx);

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
                    "Terminating instance"
                );

                // Send ack to agent before terminating so it can shut down cleanly
                if let (Some(ip), Some(tls)) = (&public_ip, &tls_config) {
                    send_ack_complete(ip, DEFAULT_GRPC_PORT, run_id.as_str(), tls).await;
                }

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

                let reporter =
                    Reporter::channel(tx.clone(), tokio_util::sync::CancellationToken::new());
                if let Err(e) = full_cleanup(FullCleanupConfig {
                    region: &cleanup_region,
                    keep,
                    bucket_name: &bucket_name,
                    instances: &instances,
                    security_group_id: security_group_id.as_deref(),
                    iam_role_name: iam_role_name.as_deref(),
                    sg_rules: &sg_rules,
                    reporter: &reporter,
                })
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

/// Parameters for a full cleanup operation.
pub struct FullCleanupConfig<'a> {
    pub region: &'a str,
    pub keep: bool,
    pub bucket_name: &'a str,
    pub instances: &'a HashMap<String, InstanceState>,
    pub security_group_id: Option<&'a str>,
    pub iam_role_name: Option<&'a str>,
    pub sg_rules: &'a [String],
    pub reporter: &'a Reporter,
}

/// Perform full cleanup of all resources for a run.
///
/// Uses in-memory resource tracking (no database) to determine what to clean up.
pub async fn full_cleanup(params: FullCleanupConfig<'_>) -> Result<()> {
    let FullCleanupConfig {
        region,
        keep,
        bucket_name,
        instances,
        security_group_id,
        iam_role_name,
        sg_rules,
        reporter,
    } = params;

    let ctx = AwsContext::new(region).await;
    let ec2 = Ec2Client::from_context(&ctx);
    let s3 = S3Client::from_context(&ctx);

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
            0,
            if iam_role_name.is_some() { 1 } else { 0 },
            sg_rules.len(),
        );
        let step = |msg: String, progress: &CleanupProgress| {
            info!("{}", msg);
            reporter.report_cleanup(progress);
        };

        // Terminate EC2 instances
        progress.current_step = format!("Terminating {} EC2 instances...", instance_ids.len());
        step(progress.current_step.clone(), &progress);
        if !instance_ids.is_empty() {
            if let Err(e) = ec2.terminate_instances(&instance_ids).await {
                warn!(error = ?e, "Failed to terminate instances in batch");
            }
        }
        progress.ec2_instances.0 = instance_ids.len();

        // Delete S3 bucket
        progress.current_step = "Deleting S3 bucket...".to_string();
        step(progress.current_step.clone(), &progress);
        if let Err(e) = s3.delete_bucket(bucket_name).await {
            warn!(bucket = %bucket_name, error = ?e, "Failed to delete bucket");
        }
        progress.s3_bucket = true;

        // Delete IAM resources
        if let Some(role_name) = iam_role_name {
            progress.current_step = "Deleting IAM role...".to_string();
            step(progress.current_step.clone(), &progress);
            let iam = IamClient::from_context(&ctx);
            if let Err(e) = iam.delete_benchmark_role(role_name).await {
                warn!(role = %role_name, error = ?e, "Failed to delete IAM role");
            }
            progress.iam_roles.0 += 1;
        }

        // Delete security group rules
        for rule in sg_rules {
            if let Some((sg_id, cidr_ip)) = rule.split_once(':') {
                if let Err(e) = ec2.remove_grpc_ingress_rule(sg_id, cidr_ip).await {
                    warn!(security_group = %sg_id, cidr_ip = %cidr_ip, error = ?e, "Failed to remove SG rule");
                }
            }
            progress.security_rules.0 += 1;
        }

        // Delete security group (must wait for instances to terminate first)
        if let Some(sg_id) = security_group_id {
            if !instance_ids.is_empty() {
                progress.current_step = "Waiting for instances to terminate...".to_string();
                step(progress.current_step.clone(), &progress);
                ec2.wait_for_all_terminated(&instance_ids).await?;
            }
            progress.current_step = "Deleting security group...".to_string();
            step(progress.current_step.clone(), &progress);
            if let Err(e) = ec2.delete_security_group(sg_id).await {
                warn!(sg_id = %sg_id, error = ?e, "Failed to delete security group");
            }
        }

        progress.current_step = "Cleanup complete".to_string();
        reporter.report_cleanup(&progress);
    } else {
        info!("Keeping instances, bucket, and IAM resources (--keep specified)");
    }

    Ok(())
}
