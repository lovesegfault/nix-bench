//! Shared cleanup utilities for AWS resources
//!
//! Provides common helpers for cleaning up AWS resources with consistent
//! error handling and ordering.

use crate::aws::{Ec2Client, IamClient, S3Client};
use super::resource_kind::ResourceKind;
use tracing::{info, warn};

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
