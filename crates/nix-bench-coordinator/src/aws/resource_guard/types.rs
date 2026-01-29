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

/// Per-variant metadata for ResourceId
struct ResourceInfo {
    priority: u8,
    kind: &'static str,
    label: &'static str,
}

impl ResourceId {
    /// Get per-variant metadata (priority, kind string, human-readable label prefix)
    fn info(&self) -> ResourceInfo {
        match self {
            ResourceId::Ec2Instance(_) => ResourceInfo {
                priority: 0,
                kind: "ec2_instance",
                label: "EC2 instance",
            },
            ResourceId::ElasticIp(_) => ResourceInfo {
                priority: 0,
                kind: "elastic_ip",
                label: "Elastic IP",
            },
            ResourceId::S3Object(_) => ResourceInfo {
                priority: 1,
                kind: "s3_object",
                label: "S3 object",
            },
            ResourceId::S3Bucket(_) => ResourceInfo {
                priority: 2,
                kind: "s3_bucket",
                label: "S3 bucket",
            },
            ResourceId::IamRole(_) => ResourceInfo {
                priority: 3,
                kind: "iam_role",
                label: "IAM role",
            },
            ResourceId::IamInstanceProfile(_) => ResourceInfo {
                priority: 3,
                kind: "iam_instance_profile",
                label: "Instance profile",
            },
            ResourceId::SecurityGroupRule { .. } => ResourceInfo {
                priority: 4,
                kind: "security_group_rule",
                label: "SG rule",
            },
            ResourceId::SecurityGroup(_) => ResourceInfo {
                priority: 5,
                kind: "security_group",
                label: "Security group",
            },
        }
    }

    /// Get cleanup priority (lower number = cleanup first)
    pub fn cleanup_priority(&self) -> u8 {
        self.info().priority
    }

    /// Get string representation of the resource kind
    pub fn as_str(&self) -> &'static str {
        self.info().kind
    }

    /// Get the raw identifier string
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
        format!("{} {}", self.info().label, self.raw_id())
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
