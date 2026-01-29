//! Background cleanup executor for dropped resources

use super::registry::{CleanupMessage, ResourceRegistry};
use super::types::{ResourceId, ResourceMeta};
use crate::aws::{Ec2Client, FromAwsContext, IamClient, S3Client};
use anyhow::Result;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Item in the cleanup queue with priority
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
        // Reverse order: lower priority number = higher priority = process first
        other
            .resource
            .cleanup_priority()
            .cmp(&self.resource.cleanup_priority())
    }
}

/// Background task that handles async cleanup of dropped resources
///
/// Resources are received via channel and cleaned up in dependency order.
pub struct CleanupExecutor {
    rx: mpsc::UnboundedReceiver<CleanupMessage>,
}

impl CleanupExecutor {
    /// Create a new cleanup executor
    pub fn new(rx: mpsc::UnboundedReceiver<CleanupMessage>) -> Self {
        Self { rx }
    }

    /// Run the cleanup executor (spawned as a background task)
    ///
    /// This will process cleanup messages until it receives a Shutdown message
    /// or the channel is closed.
    pub async fn run(mut self) {
        let mut pending: BinaryHeap<CleanupItem> = BinaryHeap::new();

        loop {
            tokio::select! {
                // Receive cleanup messages
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
                // Process pending items when channel is quiet (batch processing)
                _ = tokio::time::sleep(Duration::from_millis(100)), if !pending.is_empty() => {
                    self.process_pending(&mut pending).await;
                }
            }
        }

        // Final cleanup before shutdown
        if !pending.is_empty() {
            info!(count = pending.len(), "Processing remaining cleanup items");
            self.process_pending(&mut pending).await;
        }
    }

    async fn process_pending(&self, pending: &mut BinaryHeap<CleanupItem>) {
        // Group by region for efficient client reuse
        let mut by_region: HashMap<String, Vec<CleanupItem>> = HashMap::new();

        while let Some(item) = pending.pop() {
            by_region
                .entry(item.meta.region.clone())
                .or_default()
                .push(item);
        }

        for (region, items) in by_region {
            // Sort by priority within region
            let mut items = items;
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

        // Track instances for wait-before-SG-delete
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
            ResourceId::S3Bucket(name) => s3.delete_bucket(name).await,
            ResourceId::IamRole(name) => iam.delete_benchmark_role(name).await,
            ResourceId::IamInstanceProfile(_) => {
                // Deleted as part of role deletion
                Ok(())
            }
            ResourceId::SecurityGroupRule {
                security_group_id,
                cidr_ip,
            } => {
                ec2.remove_grpc_ingress_rule(security_group_id, cidr_ip)
                    .await
            }
            ResourceId::SecurityGroup(id) => {
                // Wait for instances to terminate first
                for instance_id in terminated_instances.iter() {
                    let _ = ec2.wait_for_terminated(instance_id).await;
                }
                ec2.delete_security_group(id).await
            }
        }
    }
}

/// Check if an error indicates the resource was not found (already deleted)
fn is_not_found_error(error_str: &str) -> bool {
    error_str.contains("NotFound")
        || error_str.contains("NoSuchBucket")
        || error_str.contains("NoSuchEntity")
        || error_str.contains("InvalidInstanceID")
        || error_str.contains("InvalidGroup")
}

/// Create a registry and executor pair
///
/// Returns the registry (for use by guards) and the executor (to be spawned).
pub fn create_cleanup_system() -> (ResourceRegistry, CleanupExecutor) {
    let (tx, rx) = mpsc::unbounded_channel();
    let registry = ResourceRegistry::new(tx);
    let executor = CleanupExecutor::new(rx);
    (registry, executor)
}
