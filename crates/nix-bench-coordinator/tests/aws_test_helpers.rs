//! Shared utilities for AWS integration tests
//!
//! Provides region detection, unique run IDs, and resource cleanup helpers.

use anyhow::Result;
use chrono::Utc;
use nix_bench_common::resource_kind::ResourceKind;
use nix_bench_coordinator::aws::{Ec2Client, IamClient, S3Client};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Get the AWS region for tests.
///
/// Checks environment variables in order:
/// 1. AWS_REGION
/// 2. AWS_DEFAULT_REGION
/// 3. Falls back to us-east-2
pub fn get_test_region() -> String {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-2".to_string())
}

/// Generate a unique run ID for test resources.
///
/// Format: `test-{timestamp}` where timestamp is Unix seconds.
/// This ensures unique resource names across test runs.
pub fn test_run_id() -> String {
    format!("test-{}", Utc::now().timestamp())
}

/// Test tag key for identifying test-created resources
pub const TEST_TAG_KEY: &str = "nix-bench:test";
/// Test tag value
pub const TEST_TAG_VALUE: &str = "true";
/// Test run ID tag key
pub const TEST_RUN_TAG_KEY: &str = "nix-bench:test-run";

/// Instance type to use for integration tests
pub const TEST_INSTANCE_TYPE: &str = "c7a.medium";

/// Timeout for instance operations (5 minutes)
pub const INSTANCE_TIMEOUT_SECS: u64 = 300;

/// Timeout for IAM propagation (2 minutes)
pub const IAM_PROPAGATION_TIMEOUT_SECS: u64 = 120;

/// Resource types that can be tracked for cleanup
#[derive(Debug, Clone)]
pub enum AwsResource {
    /// EC2 instance (must be terminated before SG can be deleted)
    Instance { id: String },
    /// Security group (depends on instances being terminated)
    SecurityGroup { id: String },
    /// IAM role (and associated instance profile)
    IamRole { name: String },
    /// S3 bucket
    S3Bucket { name: String },
}

impl AwsResource {
    /// Get the cleanup priority (lower = cleanup first)
    ///
    /// Delegates to the shared ResourceKind for consistent ordering.
    fn priority(&self) -> u8 {
        match self {
            AwsResource::Instance { .. } => ResourceKind::Ec2Instance.cleanup_priority(),
            AwsResource::S3Bucket { .. } => ResourceKind::S3Bucket.cleanup_priority(),
            AwsResource::IamRole { .. } => ResourceKind::IamRole.cleanup_priority(),
            AwsResource::SecurityGroup { .. } => ResourceKind::SecurityGroup.cleanup_priority(),
        }
    }
}

/// Clients needed for cleanup operations
pub struct TestClients {
    pub ec2: Arc<Ec2Client>,
    pub iam: Arc<IamClient>,
    pub s3: Arc<S3Client>,
}

/// Test resource tracker with automatic cleanup on drop.
///
/// Resources are cleaned up in dependency order:
/// 1. Terminate EC2 instances (and wait for termination)
/// 2. Delete Security Groups
/// 3. Delete IAM roles
/// 4. Delete S3 buckets
///
/// # Example
///
/// ```ignore
/// #[tokio::test]
/// async fn test_e2e() {
///     let resources = TestResources::with_aws(&get_test_region()).await.unwrap();
///
///     let sg_id = resources.ec2().create_security_group(...).await?;
///     resources.track(AwsResource::SecurityGroup { id: sg_id.clone() }).await;
///
///     // Test logic here...
///     // Resources automatically cleaned up when `resources` is dropped,
///     // even if the test panics.
/// }
/// ```
pub struct TestResources {
    resources: Arc<Mutex<Vec<AwsResource>>>,
    clients: Arc<TestClients>,
    runtime_handle: tokio::runtime::Handle,
}

impl TestResources {
    /// Create a new resource tracker.
    pub fn new(clients: TestClients) -> Self {
        Self {
            resources: Arc::new(Mutex::new(Vec::new())),
            clients: Arc::new(clients),
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    /// Create with real AWS clients for integration tests.
    pub async fn with_aws(region: &str) -> Result<Self> {
        let ec2 = Arc::new(Ec2Client::new(region).await?);
        let iam = Arc::new(IamClient::new(region).await?);
        let s3 = Arc::new(S3Client::new(region).await?);

        Ok(Self::new(TestClients { ec2, iam, s3 }))
    }

    /// Get the EC2 client
    pub fn ec2(&self) -> &Ec2Client {
        &self.clients.ec2
    }

    /// Get the IAM client
    pub fn iam(&self) -> &IamClient {
        &self.clients.iam
    }

    /// Get the S3 client
    pub fn s3(&self) -> &S3Client {
        &self.clients.s3
    }

    /// Track a resource for cleanup.
    pub async fn track(&self, resource: AwsResource) {
        let mut resources = self.resources.lock().await;
        resources.push(resource);
    }

    /// Remove a resource from tracking (if manually cleaned up).
    pub async fn untrack(&self, resource: &AwsResource) {
        let mut resources = self.resources.lock().await;
        resources.retain(|r| !Self::resources_match(r, resource));
    }

    fn resources_match(a: &AwsResource, b: &AwsResource) -> bool {
        match (a, b) {
            (AwsResource::Instance { id: a }, AwsResource::Instance { id: b }) => a == b,
            (AwsResource::SecurityGroup { id: a }, AwsResource::SecurityGroup { id: b }) => a == b,
            (AwsResource::IamRole { name: a }, AwsResource::IamRole { name: b }) => a == b,
            (AwsResource::S3Bucket { name: a }, AwsResource::S3Bucket { name: b }) => a == b,
            _ => false,
        }
    }

    /// Perform cleanup of all tracked resources.
    /// Called automatically on drop, but can be called manually for explicit cleanup.
    pub async fn cleanup(&self) {
        let mut resources = self.resources.lock().await;

        // Sort by priority (lower first)
        resources.sort_by_key(|r| r.priority());

        for resource in resources.drain(..) {
            if let Err(e) = self.cleanup_resource(&resource).await {
                warn!(resource = ?resource, error = ?e, "Failed to clean up test resource");
            }
        }
    }

    async fn cleanup_resource(&self, resource: &AwsResource) -> Result<()> {
        match resource {
            AwsResource::Instance { id } => {
                info!(instance_id = %id, "Cleaning up: terminating instance");
                self.clients.ec2.terminate_instance(id).await?;
                self.clients.ec2.wait_for_terminated(id).await?;
            }
            AwsResource::SecurityGroup { id } => {
                info!(sg_id = %id, "Cleaning up: deleting security group");
                // Add a small delay to ensure instance is fully detached
                tokio::time::sleep(Duration::from_secs(2)).await;
                self.clients.ec2.delete_security_group(id).await?;
            }
            AwsResource::IamRole { name } => {
                info!(role_name = %name, "Cleaning up: deleting IAM role");
                self.clients.iam.delete_benchmark_role(name).await?;
            }
            AwsResource::S3Bucket { name } => {
                info!(bucket = %name, "Cleaning up: deleting S3 bucket");
                self.clients.s3.delete_bucket(name).await?;
            }
        }
        Ok(())
    }
}

impl Drop for TestResources {
    fn drop(&mut self) {
        let resources = Arc::clone(&self.resources);
        let clients = Arc::clone(&self.clients);

        // Spawn cleanup task on the runtime
        // This is the key pattern for async cleanup in Drop
        self.runtime_handle.spawn(async move {
            let mut resources_guard = resources.lock().await;

            // Sort by priority
            resources_guard.sort_by_key(|r| r.priority());

            for resource in resources_guard.drain(..) {
                let result = match &resource {
                    AwsResource::Instance { id } => {
                        let _ = clients.ec2.terminate_instance(id).await;
                        clients.ec2.wait_for_terminated(id).await
                    }
                    AwsResource::SecurityGroup { id } => {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        clients.ec2.delete_security_group(id).await
                    }
                    AwsResource::IamRole { name } => clients.iam.delete_benchmark_role(name).await,
                    AwsResource::S3Bucket { name } => clients.s3.delete_bucket(name).await,
                };

                if let Err(e) = result {
                    eprintln!("Warning: Failed to clean up {:?}: {}", resource, e);
                }
            }
        });
    }
}
