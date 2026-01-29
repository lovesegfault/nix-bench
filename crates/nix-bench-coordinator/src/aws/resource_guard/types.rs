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
    pub fn cleanup_priority(&self) -> u8 {
        match self {
            Self::Ec2Instance(_) | Self::ElasticIp(_) => 0,
            Self::S3Object(_) => 1,
            Self::S3Bucket(_) => 2,
            Self::IamRole(_) | Self::IamInstanceProfile(_) => 3,
            Self::SecurityGroupRule { .. } => 4,
            Self::SecurityGroup(_) => 5,
        }
    }

    /// Get string representation of the resource kind
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ec2Instance(_) => "ec2_instance",
            Self::ElasticIp(_) => "elastic_ip",
            Self::S3Object(_) => "s3_object",
            Self::S3Bucket(_) => "s3_bucket",
            Self::IamRole(_) => "iam_role",
            Self::IamInstanceProfile(_) => "iam_instance_profile",
            Self::SecurityGroupRule { .. } => "security_group_rule",
            Self::SecurityGroup(_) => "security_group",
        }
    }

    /// Get the raw identifier string
    pub fn raw_id(&self) -> String {
        match self {
            Self::Ec2Instance(id)
            | Self::SecurityGroup(id)
            | Self::S3Bucket(id)
            | Self::S3Object(id)
            | Self::IamRole(id)
            | Self::IamInstanceProfile(id)
            | Self::ElasticIp(id) => id.clone(),
            Self::SecurityGroupRule {
                security_group_id,
                cidr_ip,
            } => format!("{}:{}", security_group_id, cidr_ip),
        }
    }

    /// Get a human-readable description for logging
    pub fn description(&self) -> String {
        let label = match self {
            Self::Ec2Instance(_) => "EC2 instance",
            Self::ElasticIp(_) => "Elastic IP",
            Self::S3Object(_) => "S3 object",
            Self::S3Bucket(_) => "S3 bucket",
            Self::IamRole(_) => "IAM role",
            Self::IamInstanceProfile(_) => "Instance profile",
            Self::SecurityGroupRule { .. } => "SG rule",
            Self::SecurityGroup(_) => "Security group",
        };
        format!("{} {}", label, self.raw_id())
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
