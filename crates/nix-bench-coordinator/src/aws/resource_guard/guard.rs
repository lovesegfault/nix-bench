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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::registry::CleanupMessage;
    use tokio::sync::mpsc;

    fn create_test_meta() -> ResourceMeta {
        ResourceMeta::new("test-run".to_string(), "us-east-2".to_string())
    }

    fn create_test_registry() -> (ResourceRegistry, mpsc::UnboundedReceiver<CleanupMessage>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (ResourceRegistry::new(tx), rx)
    }

    #[test]
    fn test_guard_registers_on_creation() {
        let (registry, _rx) = create_test_registry();
        let resource_id = ResourceId::Ec2Instance("i-12345".to_string());
        let meta = create_test_meta();

        // Before creating guard, registry is empty
        assert!(registry.is_empty());

        // Create guard
        let _guard = ResourceGuard::new(
            "i-12345".to_string(),
            resource_id,
            meta,
            registry.clone(),
        );

        // After creating guard, registry has 1 resource
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_guard_drop_sends_cleanup_message() {
        let (registry, mut rx) = create_test_registry();
        let resource_id = ResourceId::S3Bucket("test-bucket".to_string());
        let meta = create_test_meta();

        // Create and immediately drop guard
        {
            let _guard = ResourceGuard::new(
                "test-bucket".to_string(),
                resource_id,
                meta,
                registry.clone(),
            );
            // Guard drops here
        }

        // Registry should be empty after drop
        assert!(registry.is_empty());

        // Should have received a cleanup message
        let msg = rx.try_recv().expect("Should have cleanup message");
        match msg {
            CleanupMessage::ResourceDropped { resource, .. } => {
                match resource {
                    ResourceId::S3Bucket(name) => assert_eq!(name, "test-bucket"),
                    _ => panic!("Wrong resource type"),
                }
            }
            CleanupMessage::Shutdown => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_guard_commit_prevents_cleanup() {
        let (registry, mut rx) = create_test_registry();
        let resource_id = ResourceId::Ec2Instance("i-commit".to_string());
        let meta = create_test_meta();

        let guard = ResourceGuard::new(
            "i-commit".to_string(),
            resource_id,
            meta,
            registry.clone(),
        );

        // Registry should have the resource
        assert_eq!(registry.len(), 1);

        // Commit the guard - returns the value
        let value = guard.commit();
        assert_eq!(value, "i-commit");

        // Registry should be empty after commit
        assert!(registry.is_empty());

        // No cleanup message should have been sent
        assert!(rx.try_recv().is_err(), "No cleanup message should be sent after commit");
    }

    #[test]
    fn test_guard_detach_prevents_cleanup() {
        let (registry, mut rx) = create_test_registry();
        let resource_id = ResourceId::SecurityGroup("sg-detach".to_string());
        let meta = create_test_meta();

        let guard = ResourceGuard::new(
            "sg-detach".to_string(),
            resource_id,
            meta,
            registry.clone(),
        );

        // Detach returns the value without triggering cleanup
        let value = guard.detach();
        assert_eq!(value, "sg-detach");

        // Registry should be empty
        assert!(registry.is_empty());

        // No cleanup message
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_guard_inner_returns_value_ref() {
        let (registry, _rx) = create_test_registry();
        let resource_id = ResourceId::Ec2Instance("i-inner".to_string());
        let meta = create_test_meta();

        let guard = ResourceGuard::new(
            "i-inner".to_string(),
            resource_id,
            meta,
            registry,
        );

        assert_eq!(guard.inner(), "i-inner");

        // Commit to avoid cleanup message
        let _ = guard.commit();
    }

    #[test]
    fn test_guard_deref() {
        let (registry, _rx) = create_test_registry();
        let resource_id = ResourceId::S3Bucket("bucket-deref".to_string());
        let meta = create_test_meta();

        let guard = ResourceGuard::new(
            "bucket-deref".to_string(),
            resource_id,
            meta,
            registry,
        );

        // Can use Deref to access the value
        let len: usize = guard.len();
        assert_eq!(len, 12); // "bucket-deref".len()

        let _ = guard.commit();
    }

    #[test]
    fn test_guard_resource_id_accessor() {
        let (registry, _rx) = create_test_registry();
        let resource_id = ResourceId::IamRole("test-role".to_string());
        let meta = create_test_meta();

        let guard = ResourceGuard::new(
            ("test-role".to_string(), "test-profile".to_string()),
            resource_id.clone(),
            meta,
            registry,
        );

        assert_eq!(guard.resource_id(), &resource_id);

        let _ = guard.commit();
    }

    #[test]
    fn test_guard_meta_accessor() {
        let (registry, _rx) = create_test_registry();
        let resource_id = ResourceId::S3Bucket("meta-test".to_string());
        let meta = create_test_meta();

        let guard = ResourceGuard::new(
            "meta-test".to_string(),
            resource_id,
            meta,
            registry,
        );

        assert_eq!(guard.meta().run_id, "test-run");
        assert_eq!(guard.meta().region, "us-east-2");

        let _ = guard.commit();
    }

    #[test]
    fn test_multiple_guards_track_independently() {
        let (registry, mut rx) = create_test_registry();

        // Create 3 guards
        let guard1 = ResourceGuard::new(
            "bucket-1".to_string(),
            ResourceId::S3Bucket("bucket-1".to_string()),
            create_test_meta(),
            registry.clone(),
        );
        let guard2 = ResourceGuard::new(
            "bucket-2".to_string(),
            ResourceId::S3Bucket("bucket-2".to_string()),
            create_test_meta(),
            registry.clone(),
        );
        let guard3 = ResourceGuard::new(
            "bucket-3".to_string(),
            ResourceId::S3Bucket("bucket-3".to_string()),
            create_test_meta(),
            registry.clone(),
        );

        assert_eq!(registry.len(), 3);

        // Commit guard2 - no cleanup for it
        guard2.commit();
        assert_eq!(registry.len(), 2);

        // Drop guard1 and guard3 - should send 2 cleanup messages
        drop(guard1);
        drop(guard3);

        assert!(registry.is_empty());

        // Should have 2 cleanup messages
        let msg1 = rx.try_recv().expect("First cleanup");
        let msg2 = rx.try_recv().expect("Second cleanup");
        assert!(rx.try_recv().is_err(), "Only 2 cleanups");

        // Both should be ResourceDropped
        match (msg1, msg2) {
            (
                CleanupMessage::ResourceDropped { resource: r1, .. },
                CleanupMessage::ResourceDropped { resource: r2, .. },
            ) => {
                let ids: Vec<String> = [r1, r2]
                    .iter()
                    .map(|r| r.raw_id())
                    .collect();
                assert!(ids.contains(&"bucket-1".to_string()));
                assert!(ids.contains(&"bucket-3".to_string()));
            }
            _ => panic!("Expected ResourceDropped messages"),
        }
    }
}
