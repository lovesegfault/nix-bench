//! AWS resource types and cleanup ordering
//!
//! Provides consistent cleanup priority across all cleanup implementations.
//! Resources must be cleaned in dependency order to avoid failures.

use std::fmt;

/// Types of AWS resources managed by nix-bench
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// EC2 instance (must be terminated before SG can be deleted)
    Ec2Instance,
    /// S3 bucket
    S3Bucket,
    /// S3 object (individual object in a bucket)
    S3Object,
    /// IAM role
    IamRole,
    /// IAM instance profile
    IamInstanceProfile,
    /// Security group ingress rule
    SecurityGroupRule,
    /// Security group (depends on instances being terminated)
    SecurityGroup,
}

impl ResourceKind {
    /// Get cleanup priority (lower number = cleanup first)
    ///
    /// Resources must be cleaned up in dependency order:
    /// - 0: Terminate EC2 instances (must terminate before SG deletion)
    /// - 1: S3 objects (before bucket)
    /// - 2: Delete S3 buckets (no dependencies)
    /// - 3: Delete IAM roles/profiles (no dependencies)
    /// - 4: Delete security group rules (before SG)
    /// - 5: Delete security groups (must wait for instances to terminate)
    ///
    /// # Correct Order Rationale
    ///
    /// Security groups cannot be deleted while instances are using them.
    /// Therefore, instances (priority 0) must be cleaned before
    /// security groups (priority 5).
    pub fn cleanup_priority(self) -> u8 {
        match self {
            ResourceKind::Ec2Instance => 0,
            ResourceKind::S3Object => 1,
            ResourceKind::S3Bucket => 2,
            ResourceKind::IamRole => 3,
            ResourceKind::IamInstanceProfile => 3,
            ResourceKind::SecurityGroupRule => 4,
            ResourceKind::SecurityGroup => 5,
        }
    }

    /// Get database string representation
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::Ec2Instance => "ec2_instance",
            ResourceKind::S3Bucket => "s3_bucket",
            ResourceKind::S3Object => "s3_object",
            ResourceKind::IamRole => "iam_role",
            ResourceKind::IamInstanceProfile => "iam_instance_profile",
            ResourceKind::SecurityGroup => "security_group",
            ResourceKind::SecurityGroupRule => "security_group_rule",
        }
    }

    /// Parse from database string representation
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "ec2_instance" => Some(ResourceKind::Ec2Instance),
            "s3_bucket" => Some(ResourceKind::S3Bucket),
            "s3_object" => Some(ResourceKind::S3Object),
            "iam_role" => Some(ResourceKind::IamRole),
            "iam_instance_profile" => Some(ResourceKind::IamInstanceProfile),
            "security_group" => Some(ResourceKind::SecurityGroup),
            "security_group_rule" => Some(ResourceKind::SecurityGroupRule),
            _ => None,
        }
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instances_before_security_groups() {
        assert!(
            ResourceKind::Ec2Instance.cleanup_priority()
                < ResourceKind::SecurityGroup.cleanup_priority(),
            "Instances must be cleaned before security groups"
        );
    }

    #[test]
    fn test_sg_rules_before_sg() {
        assert!(
            ResourceKind::SecurityGroupRule.cleanup_priority()
                < ResourceKind::SecurityGroup.cleanup_priority(),
            "SG rules must be removed before SG deletion"
        );
    }

    #[test]
    fn test_priority_values() {
        assert_eq!(ResourceKind::Ec2Instance.cleanup_priority(), 0);
        assert_eq!(ResourceKind::S3Object.cleanup_priority(), 1);
        assert_eq!(ResourceKind::S3Bucket.cleanup_priority(), 2);
        assert_eq!(ResourceKind::IamRole.cleanup_priority(), 3);
        assert_eq!(ResourceKind::IamInstanceProfile.cleanup_priority(), 3);
        assert_eq!(ResourceKind::SecurityGroupRule.cleanup_priority(), 4);
        assert_eq!(ResourceKind::SecurityGroup.cleanup_priority(), 5);
    }

    #[test]
    fn test_s3_objects_before_buckets() {
        assert!(
            ResourceKind::S3Object.cleanup_priority() < ResourceKind::S3Bucket.cleanup_priority(),
            "S3 objects must be cleaned before buckets"
        );
    }

    #[test]
    fn test_as_str_and_from_str_roundtrip() {
        let kinds = [
            ResourceKind::Ec2Instance,
            ResourceKind::S3Bucket,
            ResourceKind::S3Object,
            ResourceKind::IamRole,
            ResourceKind::IamInstanceProfile,
            ResourceKind::SecurityGroup,
            ResourceKind::SecurityGroupRule,
        ];
        for kind in kinds {
            let s = kind.as_str();
            let parsed = ResourceKind::from_str(s).unwrap();
            assert_eq!(kind, parsed);
        }
    }
}
