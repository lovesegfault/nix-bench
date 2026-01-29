//! RAII guards, registry, builder, and cleanup executor for AWS resources

use super::types::{ResourceId, ResourceMeta};
use crate::aws::{Ec2Client, IamClient, S3Client};
use anyhow::Result;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ── CleanupMessage & ResourceRegistry ──────────────────────────────────────

/// Message sent to the cleanup executor when a resource needs cleanup
#[derive(Debug)]
pub enum CleanupMessage {
    /// A resource was dropped without being committed
    ResourceDropped {
        resource: ResourceId,
        meta: ResourceMeta,
    },
    /// Request the executor to shut down gracefully
    Shutdown,
}

/// Thread-safe registry of live resources that haven't been committed yet
#[derive(Clone)]
pub struct ResourceRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    resources: Mutex<HashMap<ResourceId, ResourceMeta>>,
    cleanup_tx: mpsc::UnboundedSender<CleanupMessage>,
}

impl ResourceRegistry {
    pub fn new(cleanup_tx: mpsc::UnboundedSender<CleanupMessage>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                resources: Mutex::new(HashMap::new()),
                cleanup_tx,
            }),
        }
    }

    pub fn register(&self, resource: ResourceId, meta: ResourceMeta) {
        self.inner.resources.lock().unwrap().insert(resource, meta);
    }

    pub fn commit(&self, resource: &ResourceId) -> Option<ResourceMeta> {
        self.inner.resources.lock().unwrap().remove(resource)
    }

    pub fn on_drop(&self, resource: ResourceId, meta: ResourceMeta) {
        {
            self.inner.resources.lock().unwrap().remove(&resource);
        }
        let _ = self
            .inner
            .cleanup_tx
            .send(CleanupMessage::ResourceDropped { resource, meta });
    }

    pub fn all_resources(&self) -> Vec<(ResourceId, ResourceMeta)> {
        self.inner
            .resources
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.resources.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.resources.lock().unwrap().is_empty()
    }

    pub fn shutdown(&self) {
        let _ = self.inner.cleanup_tx.send(CleanupMessage::Shutdown);
    }
}

// ── ResourceGuard ──────────────────────────────────────────────────────────

/// RAII guard that tracks an AWS resource.
///
/// When dropped without calling `commit()`, the resource is sent
/// to the cleanup executor for async deletion.
pub struct ResourceGuard<T> {
    value: ManuallyDrop<T>,
    resource_id: ResourceId,
    meta: ResourceMeta,
    registry: ResourceRegistry,
    committed: bool,
}

impl<T> ResourceGuard<T> {
    pub(crate) fn new(
        value: T,
        resource_id: ResourceId,
        meta: ResourceMeta,
        registry: ResourceRegistry,
    ) -> Self {
        registry.register(resource_id.clone(), meta.clone());
        Self {
            value: ManuallyDrop::new(value),
            resource_id,
            meta,
            registry,
            committed: false,
        }
    }

    /// Commit this resource - no cleanup on drop.
    pub fn commit(mut self) -> T {
        self.committed = true;
        self.registry.commit(&self.resource_id);
        // SAFETY: committed=true prevents Drop from using the value,
        // and we only call take() once here before forget().
        let value = unsafe { ManuallyDrop::take(&mut self.value) };
        std::mem::forget(self);
        value
    }

    pub fn inner(&self) -> &T {
        &self.value
    }

    pub fn resource_id(&self) -> &ResourceId {
        &self.resource_id
    }

    pub fn meta(&self) -> &ResourceMeta {
        &self.meta
    }

    /// Detach from registry without triggering cleanup.
    pub fn detach(mut self) -> T {
        self.committed = true;
        self.registry.commit(&self.resource_id);
        // SAFETY: Same as commit().
        let value = unsafe { ManuallyDrop::take(&mut self.value) };
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
            self.registry
                .on_drop(self.resource_id.clone(), self.meta.clone());
        }
    }
}

pub type Ec2InstanceGuard = ResourceGuard<String>;
pub type SecurityGroupGuard = ResourceGuard<String>;
pub type S3BucketGuard = ResourceGuard<String>;
pub type IamRoleGuard = ResourceGuard<(String, String)>;
pub type SecurityGroupRuleGuard = ResourceGuard<()>;

// ── ResourceGuardBuilder ───────────────────────────────────────────────────

/// Builder for creating resource guards with consistent metadata
pub struct ResourceGuardBuilder {
    registry: ResourceRegistry,
    run_id: String,
    region: String,
}

impl ResourceGuardBuilder {
    pub fn new(
        registry: ResourceRegistry,
        run_id: impl Into<String>,
        region: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            run_id: run_id.into(),
            region: region.into(),
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    fn meta(&self) -> ResourceMeta {
        ResourceMeta::new(self.run_id.clone(), self.region.clone())
    }

    pub fn ec2_instance(&self, instance_id: impl Into<String>) -> Ec2InstanceGuard {
        let id = instance_id.into();
        ResourceGuard::new(
            id.clone(),
            ResourceId::Ec2Instance(id),
            self.meta(),
            self.registry.clone(),
        )
    }

    pub fn security_group(&self, sg_id: impl Into<String>) -> SecurityGroupGuard {
        let id = sg_id.into();
        ResourceGuard::new(
            id.clone(),
            ResourceId::SecurityGroup(id),
            self.meta(),
            self.registry.clone(),
        )
    }

    pub fn s3_bucket(&self, bucket_name: impl Into<String>) -> S3BucketGuard {
        let name = bucket_name.into();
        ResourceGuard::new(
            name.clone(),
            ResourceId::S3Bucket(name),
            self.meta(),
            self.registry.clone(),
        )
    }

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

// ── CleanupExecutor ────────────────────────────────────────────────────────

struct CleanupItem {
    resource: ResourceId,
    meta: ResourceMeta,
}

impl PartialEq for CleanupItem {
    fn eq(&self, other: &Self) -> bool {
        self.resource.cleanup_priority() == other.resource.cleanup_priority()
    }
}
impl Eq for CleanupItem {}

impl PartialOrd for CleanupItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CleanupItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .resource
            .cleanup_priority()
            .cmp(&self.resource.cleanup_priority())
    }
}

/// Background task that handles async cleanup of dropped resources
pub struct CleanupExecutor {
    rx: mpsc::UnboundedReceiver<CleanupMessage>,
}

impl CleanupExecutor {
    pub fn new(rx: mpsc::UnboundedReceiver<CleanupMessage>) -> Self {
        Self { rx }
    }

    pub async fn run(mut self) {
        let mut pending: BinaryHeap<CleanupItem> = BinaryHeap::new();

        loop {
            tokio::select! {
                msg = self.rx.recv() => {
                    match msg {
                        Some(CleanupMessage::ResourceDropped { resource, meta }) => {
                            debug!(resource = %resource.description(), "Queued for cleanup");
                            pending.push(CleanupItem { resource, meta });
                        }
                        Some(CleanupMessage::Shutdown) | None => {
                            info!("Cleanup executor shutting down");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(100)), if !pending.is_empty() => {
                    self.process_pending(&mut pending).await;
                }
            }
        }

        if !pending.is_empty() {
            info!(count = pending.len(), "Processing remaining cleanup items");
            self.process_pending(&mut pending).await;
        }
    }

    async fn process_pending(&self, pending: &mut BinaryHeap<CleanupItem>) {
        let mut by_region: HashMap<String, Vec<CleanupItem>> = HashMap::new();
        while let Some(item) = pending.pop() {
            by_region
                .entry(item.meta.region.clone())
                .or_default()
                .push(item);
        }

        for (region, mut items) in by_region {
            items.sort_by_key(|i| i.resource.cleanup_priority());
            if let Err(e) = self.cleanup_region(&region, items).await {
                error!(region = %region, error = ?e, "Region cleanup failed");
            }
        }
    }

    async fn cleanup_region(&self, region: &str, items: Vec<CleanupItem>) -> Result<()> {
        let ec2 = Ec2Client::new(region).await?;
        let s3 = S3Client::new(region).await?;
        let iam = IamClient::new(region).await?;

        let mut terminated_instances: Vec<String> = Vec::new();

        for item in items {
            let result = self
                .cleanup_resource(&ec2, &s3, &iam, &item.resource, &mut terminated_instances)
                .await;

            match result {
                Ok(()) => {
                    info!(
                        resource = %item.resource.description(),
                        run_id = %item.meta.run_id,
                        "Cleaned up dropped resource"
                    );
                }
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if is_not_found_error(&error_str) {
                        debug!(
                            resource = %item.resource.description(),
                            "Resource already deleted"
                        );
                    } else {
                        warn!(
                            resource = %item.resource.description(),
                            error = ?e,
                            "Failed to cleanup dropped resource"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    async fn cleanup_resource(
        &self,
        ec2: &Ec2Client,
        s3: &S3Client,
        iam: &IamClient,
        resource: &ResourceId,
        terminated_instances: &mut Vec<String>,
    ) -> Result<()> {
        match resource {
            ResourceId::Ec2Instance(id) => {
                terminated_instances.push(id.clone());
                ec2.terminate_instance(id).await
            }
            ResourceId::ElasticIp(id) => ec2.release_elastic_ip(id).await,
            ResourceId::S3Bucket(name) => s3.delete_bucket(name).await,
            ResourceId::S3Object(_) => Ok(()),
            ResourceId::IamRole(name) => iam.delete_benchmark_role(name).await,
            ResourceId::IamInstanceProfile(_) => Ok(()),
            ResourceId::SecurityGroupRule {
                security_group_id,
                cidr_ip,
            } => {
                ec2.remove_grpc_ingress_rule(security_group_id, cidr_ip)
                    .await
            }
            ResourceId::SecurityGroup(id) => {
                for instance_id in terminated_instances.iter() {
                    let _ = ec2.wait_for_terminated(instance_id).await;
                }
                ec2.delete_security_group(id).await
            }
        }
    }
}

fn is_not_found_error(error_str: &str) -> bool {
    error_str.contains("NotFound")
        || error_str.contains("NoSuchBucket")
        || error_str.contains("NoSuchEntity")
        || error_str.contains("InvalidInstanceID")
        || error_str.contains("InvalidGroup")
}

/// Create a registry and executor pair
pub fn create_cleanup_system() -> (ResourceRegistry, CleanupExecutor) {
    let (tx, rx) = mpsc::unbounded_channel();
    let registry = ResourceRegistry::new(tx);
    let executor = CleanupExecutor::new(rx);
    (registry, executor)
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert!(registry.is_empty());
        let _guard = ResourceGuard::new("i-12345".to_string(), resource_id, meta, registry.clone());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_guard_drop_sends_cleanup_message() {
        let (registry, mut rx) = create_test_registry();
        let resource_id = ResourceId::S3Bucket("test-bucket".to_string());
        let meta = create_test_meta();

        {
            let _guard = ResourceGuard::new(
                "test-bucket".to_string(),
                resource_id,
                meta,
                registry.clone(),
            );
        }

        assert!(registry.is_empty());
        let msg = rx.try_recv().expect("Should have cleanup message");
        match msg {
            CleanupMessage::ResourceDropped { resource, .. } => match resource {
                ResourceId::S3Bucket(name) => assert_eq!(name, "test-bucket"),
                _ => panic!("Wrong resource type"),
            },
            CleanupMessage::Shutdown => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_guard_commit_prevents_cleanup() {
        let (registry, mut rx) = create_test_registry();
        let resource_id = ResourceId::Ec2Instance("i-commit".to_string());
        let meta = create_test_meta();

        let guard = ResourceGuard::new("i-commit".to_string(), resource_id, meta, registry.clone());
        assert_eq!(registry.len(), 1);

        let value = guard.commit();
        assert_eq!(value, "i-commit");
        assert!(registry.is_empty());
        assert!(
            rx.try_recv().is_err(),
            "No cleanup message should be sent after commit"
        );
    }

    #[test]
    fn test_guard_detach_prevents_cleanup() {
        let (registry, mut rx) = create_test_registry();
        let resource_id = ResourceId::SecurityGroup("sg-detach".to_string());
        let meta = create_test_meta();

        let guard =
            ResourceGuard::new("sg-detach".to_string(), resource_id, meta, registry.clone());
        let value = guard.detach();
        assert_eq!(value, "sg-detach");
        assert!(registry.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_multiple_guards_track_independently() {
        let (registry, mut rx) = create_test_registry();

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
        guard2.commit();
        assert_eq!(registry.len(), 2);

        drop(guard1);
        drop(guard3);
        assert!(registry.is_empty());

        let msg1 = rx.try_recv().expect("First cleanup");
        let msg2 = rx.try_recv().expect("Second cleanup");
        assert!(rx.try_recv().is_err(), "Only 2 cleanups");

        match (msg1, msg2) {
            (
                CleanupMessage::ResourceDropped { resource: r1, .. },
                CleanupMessage::ResourceDropped { resource: r2, .. },
            ) => {
                let ids: Vec<String> = [r1, r2].iter().map(|r| r.raw_id()).collect();
                assert!(ids.contains(&"bucket-1".to_string()));
                assert!(ids.contains(&"bucket-3".to_string()));
            }
            _ => panic!("Expected ResourceDropped messages"),
        }
    }
}
