//! RAII guard for AWS resources

use super::registry::ResourceRegistry;
use super::types::{ResourceId, ResourceMeta};
use std::ops::Deref;

/// RAII guard that tracks an AWS resource
///
/// When dropped without calling `commit()`, the resource is sent
/// to the cleanup executor for async deletion.
///
/// # Example
///
/// ```ignore
/// let registry = ResourceRegistry::new(cleanup_tx);
/// let builder = ResourceGuardBuilder::new(registry, "run-123", "us-east-2");
///
/// // Create resource and guard
/// let instance_id = ec2.launch_instance(...).await?;
/// let guard = builder.ec2_instance(instance_id.clone());
///
/// // If we crash here, the instance will be cleaned up by the executor
///
/// // After recording to DB, commit the guard
/// db.insert_resource(ResourceType::Ec2Instance, &instance_id).await?;
/// let instance_id = guard.commit(); // No cleanup will happen
/// ```
pub struct ResourceGuard<T> {
    /// The wrapped value (e.g., instance_id String)
    value: T,
    /// Resource identifier for cleanup
    resource_id: ResourceId,
    /// Metadata for cleanup
    meta: ResourceMeta,
    /// Registry to notify on drop
    registry: ResourceRegistry,
    /// Whether this resource has been committed (no cleanup needed)
    committed: bool,
}

impl<T> ResourceGuard<T> {
    /// Create a new guard (internal - use `ResourceGuardBuilder`)
    pub(crate) fn new(
        value: T,
        resource_id: ResourceId,
        meta: ResourceMeta,
        registry: ResourceRegistry,
    ) -> Self {
        // Register immediately upon creation
        registry.register(resource_id.clone(), meta.clone());

        Self {
            value,
            resource_id,
            meta,
            registry,
            committed: false,
        }
    }

    /// Commit this resource - transfers ownership to persistent storage
    ///
    /// After commit, drop will NOT trigger cleanup. Call this after
    /// the resource has been successfully recorded in the database.
    pub fn commit(mut self) -> T {
        self.committed = true;
        self.registry.commit(&self.resource_id);

        // Move out value without running Drop cleanup
        // SAFETY: We set committed=true and forget self, so Drop won't run
        let value = unsafe { std::ptr::read(&self.value) };
        std::mem::forget(self);
        value
    }

    /// Get the inner value without consuming
    pub fn inner(&self) -> &T {
        &self.value
    }

    /// Get the resource ID
    pub fn resource_id(&self) -> &ResourceId {
        &self.resource_id
    }

    /// Get the resource metadata
    pub fn meta(&self) -> &ResourceMeta {
        &self.meta
    }

    /// Detach from registry without triggering cleanup
    ///
    /// Use when the resource has been cleaned up explicitly and you want
    /// to consume the guard without triggering another cleanup.
    pub fn detach(mut self) -> T {
        self.committed = true;
        self.registry.commit(&self.resource_id);

        let value = unsafe { std::ptr::read(&self.value) };
        std::mem::forget(self);
        value
    }
}

impl<T> Deref for ResourceGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> Drop for ResourceGuard<T> {
    fn drop(&mut self) {
        if !self.committed {
            // Resource was dropped without commit - needs cleanup
            self.registry
                .on_drop(self.resource_id.clone(), self.meta.clone());
        }
    }
}

// Type aliases for common resource types
pub type Ec2InstanceGuard = ResourceGuard<String>;
pub type SecurityGroupGuard = ResourceGuard<String>;
pub type S3BucketGuard = ResourceGuard<String>;
pub type IamRoleGuard = ResourceGuard<(String, String)>; // (role_name, profile_name)
pub type SecurityGroupRuleGuard = ResourceGuard<()>;
