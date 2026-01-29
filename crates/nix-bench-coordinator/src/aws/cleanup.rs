//! Tag-based AWS resource cleanup
//!
//! Discovers orphaned resources via AWS tags and cleans them up.
//! This provides a safety net for resources that were created but never
//! recorded in the local database (e.g., due to crashes).

use super::resource_kind::ResourceKind;
use super::scanner::{DiscoveredResource, ResourceScanner, ScanConfig};
use crate::aws::context::AwsContext;
use crate::aws::{Ec2Client, IamClient, S3Client};
use anyhow::Result;
use chrono::Duration;
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Cleanup configuration
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// Minimum age before considering a resource orphaned
    pub min_age: Duration,
    /// Only clean up resources from this run
    pub run_id: Option<String>,
    /// Actually delete resources (false = dry run)
    pub dry_run: bool,
    /// Force deletion even for resources in "creating" status
    pub force: bool,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            min_age: Duration::hours(1),
            run_id: None,
            dry_run: true,
            force: false,
        }
    }
}

/// Report of cleanup operations
#[derive(Default, Debug)]
pub struct CleanupReport {
    pub total_found: usize,
    pub ec2_instances: usize,
    pub s3_buckets: usize,
    pub iam_roles: usize,
    pub iam_instance_profiles: usize,
    pub security_groups: usize,
    pub deleted: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Tag-based resource cleanup
pub struct TagBasedCleanup {
    scanner: ResourceScanner,
    ec2: Ec2Client,
    s3: S3Client,
    iam: IamClient,
    region: String,
}

impl TagBasedCleanup {
    /// Create a new tag-based cleanup instance
    pub async fn new(region: &str) -> Result<Self> {
        let ctx = AwsContext::new(region).await;
        Self::from_context(&ctx)
    }

    /// Create a tag-based cleanup instance from a shared AWS context.
    ///
    /// This avoids creating redundant AWS SDK clients when the caller
    /// already has an `AwsContext`.
    pub fn from_context(ctx: &AwsContext) -> Result<Self> {
        Ok(Self {
            scanner: ResourceScanner::from_context(ctx),
            ec2: Ec2Client::from_context(ctx),
            s3: S3Client::from_context(ctx),
            iam: IamClient::from_context(ctx),
            region: ctx.region().to_string(),
        })
    }

    /// Scan and optionally clean up orphaned resources
    pub async fn cleanup(&self, config: &CleanupConfig) -> Result<CleanupReport> {
        let scan_config = ScanConfig {
            min_age: config.min_age,
            run_id: config.run_id.clone(),
            include_creating: config.force,
            ..Default::default()
        };

        info!(
            min_age_hours = config.min_age.num_hours(),
            dry_run = config.dry_run,
            run_id = ?config.run_id,
            region = %self.region,
            "Scanning for orphaned resources"
        );

        let resources = self.scanner.scan_all(&scan_config).await?;

        let mut report = CleanupReport {
            total_found: resources.len(),
            ..Default::default()
        };

        if resources.is_empty() {
            info!("No orphaned resources found");
            return Ok(report);
        }

        info!(
            count = resources.len(),
            "Found potential orphaned resources"
        );

        // Group by run_id for organized cleanup
        let by_run: HashMap<String, Vec<&DiscoveredResource>> =
            resources.iter().fold(HashMap::new(), |mut acc, r| {
                acc.entry(r.run_id.clone()).or_default().push(r);
                acc
            });

        for (run_id, run_resources) in by_run {
            info!(
                run_id = %run_id,
                count = run_resources.len(),
                "Processing run"
            );

            // Sort by cleanup priority
            let mut sorted_resources = run_resources;
            sorted_resources.sort_by_key(|r| r.resource_type.cleanup_priority());

            // Track instances for wait-before-SG-delete
            let mut terminated_instances: Vec<String> = Vec::new();

            for resource in sorted_resources {
                // Update per-type counters
                match resource.resource_type {
                    ResourceKind::Ec2Instance => report.ec2_instances += 1,
                    ResourceKind::S3Bucket => report.s3_buckets += 1,
                    ResourceKind::IamRole => report.iam_roles += 1,
                    ResourceKind::IamInstanceProfile => report.iam_instance_profiles += 1,
                    ResourceKind::SecurityGroup => report.security_groups += 1,
                    ResourceKind::S3Object
                    | ResourceKind::SecurityGroupRule
                    | ResourceKind::ElasticIp => {
                        debug!(
                            resource_id = %resource.resource_id,
                            resource_type = ?resource.resource_type,
                            "Skipping (not discoverable)"
                        );
                        continue;
                    }
                }

                if config.dry_run {
                    info!(
                        resource_type = %resource.resource_type.as_str(),
                        resource_id = %resource.resource_id,
                        "[DRY RUN] Would delete"
                    );
                    report.skipped += 1;
                    continue;
                }

                // Security groups need instances to terminate first
                if resource.resource_type == ResourceKind::SecurityGroup
                    && !terminated_instances.is_empty()
                {
                    for instance_id in &terminated_instances {
                        let _ = self.ec2.wait_for_terminated(instance_id).await;
                    }
                    terminated_instances.clear();
                }

                match delete_resource(
                    resource.resource_type,
                    &resource.resource_id,
                    &self.ec2,
                    &self.s3,
                    &self.iam,
                )
                .await
                {
                    CleanupResult::Deleted => {
                        if resource.resource_type == ResourceKind::Ec2Instance {
                            terminated_instances.push(resource.resource_id.clone());
                        }
                        report.deleted += 1;
                    }
                    CleanupResult::AlreadyDeleted => report.deleted += 1,
                    CleanupResult::Failed => report.failed += 1,
                    CleanupResult::Skipped => report.skipped += 1,
                }
            }
        }

        Ok(report)
    }
}

// ── Shared cleanup utilities ───────────────────────────────────────────────

/// Result of a single resource cleanup operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupResult {
    /// Resource was successfully deleted
    Deleted,
    /// Resource was already deleted (not found)
    AlreadyDeleted,
    /// Cleanup failed with error
    Failed,
    /// Resource was skipped (dry run or not applicable)
    Skipped,
}

/// Delete a single resource and handle "not found" errors gracefully.
pub async fn delete_resource(
    resource_type: ResourceKind,
    resource_id: &str,
    ec2: &Ec2Client,
    s3: &S3Client,
    iam: &IamClient,
) -> CleanupResult {
    let result = match resource_type {
        ResourceKind::Ec2Instance => ec2.terminate_instance(resource_id).await,
        ResourceKind::ElasticIp => ec2.release_elastic_ip(resource_id).await,
        ResourceKind::S3Bucket => s3.delete_bucket(resource_id).await,
        ResourceKind::S3Object => return CleanupResult::Skipped, // Cleaned with bucket
        ResourceKind::IamRole => iam.delete_benchmark_role(resource_id).await,
        ResourceKind::IamInstanceProfile => iam.delete_instance_profile(resource_id).await,
        ResourceKind::SecurityGroup => ec2.delete_security_group(resource_id).await,
        ResourceKind::SecurityGroupRule => {
            if let Some((sg_id, cidr_ip)) = resource_id.split_once(':') {
                ec2.remove_grpc_ingress_rule(sg_id, cidr_ip).await
            } else {
                warn!(resource_id = %resource_id, "Invalid SecurityGroupRule format");
                return CleanupResult::Failed;
            }
        }
    };

    match result {
        Ok(()) => {
            info!(resource_type = %resource_type.as_str(), resource_id = %resource_id, "Deleted");
            CleanupResult::Deleted
        }
        Err(e) => {
            warn!(
                resource_type = %resource_type.as_str(),
                resource_id = %resource_id,
                error = ?e,
                "Cleanup failed"
            );
            CleanupResult::Failed
        }
    }
}

/// Partition resources into cleanup order: instances first, then others, then security groups.
///
/// Returns (instances, non_sg_resources, security_groups)
pub fn partition_resources_for_cleanup<T, F>(
    resources: Vec<T>,
    get_type: F,
) -> (Vec<T>, Vec<T>, Vec<T>)
where
    F: Fn(&T) -> ResourceKind,
{
    let (instances, rest): (Vec<_>, Vec<_>) = resources
        .into_iter()
        .partition(|r| get_type(r) == ResourceKind::Ec2Instance);

    let (security_groups, other): (Vec<_>, Vec<_>) = rest
        .into_iter()
        .partition(|r| get_type(r) == ResourceKind::SecurityGroup);

    (instances, other, security_groups)
}
