//! Builder for creating resource guards with proper metadata

use super::guard::*;
use super::registry::ResourceRegistry;
use super::types::{ResourceId, ResourceMeta};

/// Builder for creating resource guards with consistent metadata
///
/// # Example
///
/// ```ignore
/// let builder = ResourceGuardBuilder::new(registry, "run-123", "us-east-2");
///
/// // Create guards for resources
/// let bucket_guard = builder.s3_bucket("my-bucket");
/// let instance_guard = builder.ec2_instance("i-12345678");
///
/// // After recording to DB, commit the guards
/// bucket_guard.commit();
/// instance_guard.commit();
/// ```
pub struct ResourceGuardBuilder {
    registry: ResourceRegistry,
    run_id: String,
    region: String,
}

impl ResourceGuardBuilder {
    /// Create a new builder with the given registry and run context
    pub fn new(registry: ResourceRegistry, run_id: impl Into<String>, region: impl Into<String>) -> Self {
        Self {
            registry,
            run_id: run_id.into(),
            region: region.into(),
        }
    }

    /// Get the run ID
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Get the region
    pub fn region(&self) -> &str {
        &self.region
    }

    fn meta(&self) -> ResourceMeta {
        ResourceMeta::new(self.run_id.clone(), self.region.clone())
    }

    /// Wrap an EC2 instance ID
    pub fn ec2_instance(&self, instance_id: impl Into<String>) -> Ec2InstanceGuard {
        let id = instance_id.into();
        ResourceGuard::new(
            id.clone(),
            ResourceId::Ec2Instance(id),
            self.meta(),
            self.registry.clone(),
        )
    }

    /// Wrap a security group ID
    pub fn security_group(&self, sg_id: impl Into<String>) -> SecurityGroupGuard {
        let id = sg_id.into();
        ResourceGuard::new(
            id.clone(),
            ResourceId::SecurityGroup(id),
            self.meta(),
            self.registry.clone(),
        )
    }

    /// Wrap an S3 bucket
    pub fn s3_bucket(&self, bucket_name: impl Into<String>) -> S3BucketGuard {
        let name = bucket_name.into();
        ResourceGuard::new(
            name.clone(),
            ResourceId::S3Bucket(name),
            self.meta(),
            self.registry.clone(),
        )
    }

    /// Wrap IAM role and instance profile (created together)
    pub fn iam_role(
        &self,
        role_name: impl Into<String>,
        profile_name: impl Into<String>,
    ) -> IamRoleGuard {
        let role = role_name.into();
        let profile = profile_name.into();
        ResourceGuard::new(
            (role.clone(), profile),
            ResourceId::IamRole(role),
            self.meta(),
            self.registry.clone(),
        )
    }

    /// Wrap a security group rule (for user-provided SGs where we add our own rule)
    pub fn security_group_rule(
        &self,
        security_group_id: impl Into<String>,
        cidr_ip: impl Into<String>,
    ) -> SecurityGroupRuleGuard {
        let sg_id = security_group_id.into();
        let cidr = cidr_ip.into();
        ResourceGuard::new(
            (),
            ResourceId::SecurityGroupRule {
                security_group_id: sg_id,
                cidr_ip: cidr,
            },
            self.meta(),
            self.registry.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn test_builder_creates_guards() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ResourceRegistry::new(tx);
        let builder = ResourceGuardBuilder::new(registry.clone(), "run-123", "us-east-2");

        // Test EC2 instance guard
        let guard = builder.ec2_instance("i-12345678");
        assert_eq!(*guard, "i-12345678");
        guard.commit();

        // Test S3 bucket guard
        let guard = builder.s3_bucket("my-bucket");
        assert_eq!(*guard, "my-bucket");
        guard.commit();

        // Test IAM role guard
        let guard = builder.iam_role("my-role", "my-profile");
        assert_eq!(
            guard.inner(),
            &("my-role".to_string(), "my-profile".to_string())
        );
        guard.commit();

        // Registry should be empty after all commits
        assert!(registry.is_empty());
    }
}
