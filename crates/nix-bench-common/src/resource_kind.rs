//! AWS resource types and cleanup ordering
//!
//! Provides consistent cleanup priority across all cleanup implementations.
//! Resources must be cleaned in dependency order to avoid failures.

/// Types of AWS resources managed by nix-bench
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// EC2 instance (must be terminated before SG can be deleted)
    Ec2Instance,
    /// S3 bucket
    S3Bucket,
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
    /// - 1: Delete S3 buckets (no dependencies)
    /// - 2: Delete IAM roles/profiles (no dependencies)
    /// - 3: Delete security group rules (before SG)
    /// - 4: Delete security groups (must wait for instances to terminate)
    ///
    /// # Correct Order Rationale
    ///
    /// Security groups cannot be deleted while instances are using them.
    /// Therefore, instances (priority 0) must be cleaned before
    /// security groups (priority 4).
    pub fn cleanup_priority(self) -> u8 {
        match self {
            ResourceKind::Ec2Instance => 0,
            ResourceKind::S3Bucket => 1,
            ResourceKind::IamRole => 2,
            ResourceKind::IamInstanceProfile => 2,
            ResourceKind::SecurityGroupRule => 3,
            ResourceKind::SecurityGroup => 4,
        }
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
        assert_eq!(ResourceKind::S3Bucket.cleanup_priority(), 1);
        assert_eq!(ResourceKind::IamRole.cleanup_priority(), 2);
        assert_eq!(ResourceKind::IamInstanceProfile.cleanup_priority(), 2);
        assert_eq!(ResourceKind::SecurityGroupRule.cleanup_priority(), 3);
        assert_eq!(ResourceKind::SecurityGroup.cleanup_priority(), 4);
    }
}
