//! Core types for resource tracking and cleanup

use chrono::{DateTime, Utc};
use std::fmt;

/// Identifies an AWS resource uniquely for tracking and cleanup.
///
/// Carries both the resource kind and its identifier.
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
    /// S3 object (individual object in a bucket)
    S3Object(String),
    /// IAM role
    IamRole(String),
    /// IAM instance profile
    IamInstanceProfile(String),
    /// Elastic IP address
    ElasticIp(String),
}

impl ResourceId {
    /// Get cleanup priority (lower number = cleanup first)
    ///
    /// Resources must be cleaned up in dependency order:
    /// - 0: Terminate EC2 instances / release Elastic IPs
    /// - 1: S3 objects (before bucket)
    /// - 2: Delete S3 buckets
    /// - 3: Delete IAM roles/profiles
    /// - 4: Delete security group rules (before SG)
    /// - 5: Delete security groups (must wait for instances to terminate)
    pub fn cleanup_priority(&self) -> u8 {
        match self {
            ResourceId::Ec2Instance(_) | ResourceId::ElasticIp(_) => 0,
            ResourceId::S3Object(_) => 1,
            ResourceId::S3Bucket(_) => 2,
            ResourceId::IamRole(_) | ResourceId::IamInstanceProfile(_) => 3,
            ResourceId::SecurityGroupRule { .. } => 4,
            ResourceId::SecurityGroup(_) => 5,
        }
    }

    /// Get string representation of the resource kind
    pub fn as_str(&self) -> &'static str {
        match self {
            ResourceId::Ec2Instance(_) => "ec2_instance",
            ResourceId::S3Bucket(_) => "s3_bucket",
            ResourceId::S3Object(_) => "s3_object",
            ResourceId::IamRole(_) => "iam_role",
            ResourceId::IamInstanceProfile(_) => "iam_instance_profile",
            ResourceId::SecurityGroup(_) => "security_group",
            ResourceId::SecurityGroupRule { .. } => "security_group_rule",
            ResourceId::ElasticIp(_) => "elastic_ip",
        }
    }

    /// Get the raw identifier string for database storage
    pub fn raw_id(&self) -> String {
        match self {
            ResourceId::Ec2Instance(id)
            | ResourceId::SecurityGroup(id)
            | ResourceId::S3Bucket(id)
            | ResourceId::S3Object(id)
            | ResourceId::IamRole(id)
            | ResourceId::IamInstanceProfile(id)
            | ResourceId::ElasticIp(id) => id.clone(),
            ResourceId::SecurityGroupRule {
                security_group_id,
                cidr_ip,
            } => format!("{}:{}", security_group_id, cidr_ip),
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
            ResourceId::S3Object(key) => format!("S3 object {}", key),
            ResourceId::IamRole(name) => format!("IAM role {}", name),
            ResourceId::IamInstanceProfile(name) => format!("Instance profile {}", name),
            ResourceId::ElasticIp(id) => format!("Elastic IP {}", id),
        }
    }

    /// Check if this is an EC2 instance
    pub fn is_ec2_instance(&self) -> bool {
        matches!(self, ResourceId::Ec2Instance(_))
    }

    /// Check if this is a security group
    pub fn is_security_group(&self) -> bool {
        matches!(self, ResourceId::SecurityGroup(_))
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
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
