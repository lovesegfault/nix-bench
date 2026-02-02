//! Resource cleanup functions
//!
//! This module handles cleanup of AWS resources (EC2 instances, S3 buckets,
//! IAM roles, security groups) after benchmark runs.

use std::collections::HashMap;

use anyhow::Result;
use tracing::{info, warn};

use super::progress::Reporter;
use super::types::{InstanceState, InstanceStatus};
use crate::aws::context::AwsContext;
use crate::aws::{Ec2Client, IamClient, S3Client};
use crate::tui::CleanupProgress;

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
