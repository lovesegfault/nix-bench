//! Core types for resource tracking and cleanup

use chrono::{DateTime, Utc};
use nix_bench_common::resource_kind::ResourceKind;

/// Identifies an AWS resource uniquely for tracking and cleanup
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceId {
    /// EC2 instance (must be terminated before SG can be deleted)
    Ec2Instance(String),
    /// Security group (depends on instances being terminated)
    SecurityGroup(String),
    /// Security group ingress rule (compound: sg_id:cidr)
    SecurityGroupRule {
        security_group_id: String,
        cidr_ip: String,
    },
    /// S3 bucket
    S3Bucket(String),
    /// IAM role
    IamRole(String),
    /// IAM instance profile
    IamInstanceProfile(String),
}

impl ResourceId {
    /// Get cleanup priority (lower = cleanup first)
    ///
    /// Delegates to the shared ResourceKind for consistent ordering across
    /// all cleanup implementations.
    pub fn cleanup_priority(&self) -> u8 {
        match self {
            ResourceId::Ec2Instance(_) => ResourceKind::Ec2Instance.cleanup_priority(),
            ResourceId::S3Bucket(_) => ResourceKind::S3Bucket.cleanup_priority(),
            ResourceId::IamRole(_) => ResourceKind::IamRole.cleanup_priority(),
            ResourceId::IamInstanceProfile(_) => ResourceKind::IamInstanceProfile.cleanup_priority(),
            ResourceId::SecurityGroupRule { .. } => ResourceKind::SecurityGroupRule.cleanup_priority(),
            ResourceId::SecurityGroup(_) => ResourceKind::SecurityGroup.cleanup_priority(),
        }
    }

    /// Get the raw identifier string for database storage
    pub fn raw_id(&self) -> String {
        match self {
            ResourceId::Ec2Instance(id) => id.clone(),
            ResourceId::SecurityGroup(id) => id.clone(),
            ResourceId::SecurityGroupRule {
                security_group_id,
                cidr_ip,
            } => format!("{}:{}", security_group_id, cidr_ip),
            ResourceId::S3Bucket(name) => name.clone(),
            ResourceId::IamRole(name) => name.clone(),
            ResourceId::IamInstanceProfile(name) => name.clone(),
        }
    }

    /// Get a human-readable description for logging
    pub fn description(&self) -> String {
        match self {
            ResourceId::Ec2Instance(id) => format!("EC2 instance {}", id),
            ResourceId::SecurityGroup(id) => format!("Security group {}", id),
            ResourceId::SecurityGroupRule {
                security_group_id,
                cidr_ip,
            } => format!("SG rule {}:{}", security_group_id, cidr_ip),
            ResourceId::S3Bucket(name) => format!("S3 bucket {}", name),
            ResourceId::IamRole(name) => format!("IAM role {}", name),
            ResourceId::IamInstanceProfile(name) => format!("Instance profile {}", name),
        }
    }
}

/// Metadata about a tracked resource
#[derive(Debug, Clone)]
pub struct ResourceMeta {
    /// Run ID this resource belongs to
    pub run_id: String,
    /// AWS region where the resource was created
    pub region: String,
    /// When the resource was created
    pub created_at: DateTime<Utc>,
}

impl ResourceMeta {
    /// Create new metadata for a resource being created now
    pub fn new(run_id: String, region: String) -> Self {
        Self {
            run_id,
            region,
            created_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cleanup_priority_order() {
        // Instances must be cleaned up before security groups
        assert!(
            ResourceId::Ec2Instance("i-123".into()).cleanup_priority()
                < ResourceId::SecurityGroup("sg-123".into()).cleanup_priority()
        );

        // S3 buckets after instances but before SGs
        assert!(
            ResourceId::S3Bucket("bucket".into()).cleanup_priority()
                > ResourceId::Ec2Instance("i-123".into()).cleanup_priority()
        );
        assert!(
            ResourceId::S3Bucket("bucket".into()).cleanup_priority()
                < ResourceId::SecurityGroup("sg-123".into()).cleanup_priority()
        );
    }

    #[test]
    fn test_raw_id() {
        assert_eq!(
            ResourceId::Ec2Instance("i-123".into()).raw_id(),
            "i-123"
        );
        assert_eq!(
            ResourceId::SecurityGroupRule {
                security_group_id: "sg-123".into(),
                cidr_ip: "10.0.0.1/32".into()
            }
            .raw_id(),
            "sg-123:10.0.0.1/32"
        );
    }
}
