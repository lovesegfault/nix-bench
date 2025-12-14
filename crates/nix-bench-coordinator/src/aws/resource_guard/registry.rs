//! Thread-safe resource registry for tracking AWS resources

use super::types::{ResourceId, ResourceMeta};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Message sent to the cleanup executor when a resource needs cleanup
#[derive(Debug)]
pub enum CleanupMessage {
    /// A resource was dropped without being committed - needs cleanup
    ResourceDropped {
        resource: ResourceId,
        meta: ResourceMeta,
    },
    /// Request the executor to shut down gracefully
    Shutdown,
}

/// Thread-safe registry of live resources that haven't been committed yet
///
/// Resources are registered immediately upon creation. If the process crashes
/// or the guard is dropped without calling `commit()`, the resource is sent
/// to the cleanup executor for deletion.
#[derive(Clone)]
pub struct ResourceRegistry {
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    /// Currently tracked resources (not yet committed or cleaned)
    resources: Mutex<HashMap<ResourceId, ResourceMeta>>,
    /// Channel to send cleanup requests to the executor
    cleanup_tx: mpsc::UnboundedSender<CleanupMessage>,
}

impl ResourceRegistry {
    /// Create a new registry with a cleanup channel
    pub fn new(cleanup_tx: mpsc::UnboundedSender<CleanupMessage>) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                resources: Mutex::new(HashMap::new()),
                cleanup_tx,
            }),
        }
    }

    /// Register a resource for tracking
    ///
    /// This should be called immediately after the AWS resource is created,
    /// before any fallible operations that could leave the resource orphaned.
    pub fn register(&self, resource: ResourceId, meta: ResourceMeta) {
        let mut resources = self.inner.resources.lock().unwrap();
        resources.insert(resource, meta);
    }

    /// Commit a resource - removes from registry without triggering cleanup
    ///
    /// Call this after the resource has been successfully recorded in the database.
    /// Returns the metadata if the resource was tracked.
    pub fn commit(&self, resource: &ResourceId) -> Option<ResourceMeta> {
        let mut resources = self.inner.resources.lock().unwrap();
        resources.remove(resource)
    }

    /// Called when a guard is dropped without commit
    ///
    /// Removes the resource from tracking and sends it to the cleanup executor.
    pub fn on_drop(&self, resource: ResourceId, meta: ResourceMeta) {
        // Remove from registry
        {
            let mut resources = self.inner.resources.lock().unwrap();
            resources.remove(&resource);
        }

        // Send to cleanup executor (ignore send errors - executor may have shut down)
        let _ = self
            .inner
            .cleanup_tx
            .send(CleanupMessage::ResourceDropped { resource, meta });
    }

    /// Get all currently tracked resources
    ///
    /// This is useful for emergency cleanup on shutdown.
    pub fn all_resources(&self) -> Vec<(ResourceId, ResourceMeta)> {
        let resources = self.inner.resources.lock().unwrap();
        resources
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Get the count of currently tracked resources
    pub fn len(&self) -> usize {
        self.inner.resources.lock().unwrap().len()
    }

    /// Check if there are no tracked resources
    pub fn is_empty(&self) -> bool {
        self.inner.resources.lock().unwrap().is_empty()
    }

    /// Request the cleanup executor to shut down
    pub fn shutdown(&self) {
        let _ = self.inner.cleanup_tx.send(CleanupMessage::Shutdown);
    }
}
