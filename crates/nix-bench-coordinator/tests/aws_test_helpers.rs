//! Shared utilities for AWS integration tests

#![allow(dead_code)]

use chrono::Utc;
use std::sync::atomic::{AtomicU32, Ordering};

pub fn get_test_region() -> String {
    std::env::var("AWS_REGION")
        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
        .unwrap_or_else(|_| "us-east-2".to_string())
}

pub fn test_run_id() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let ts = Utc::now().timestamp_millis();
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("test-{}-{}", ts, counter)
}

use nix_bench_coordinator::aws::cleanup::{CleanupConfig, TagBasedCleanup};
use nix_bench_coordinator::aws::context::AwsContext;
use nix_bench_coordinator::aws::{Ec2Client, IamClient, S3Client};
use std::time::Duration;

/// Clean up stale nix-bench resources from previous failed test runs.
///
/// Uses the same tag-based scanner as `cleanup-orphans --execute --min-age-hours 0`.
/// Since all nix-bench resources are tagged via the production create methods,
/// this finds and deletes any orphaned resources regardless of age.
pub async fn cleanup_stale_test_resources(region: &str) {
    let cleanup = match TagBasedCleanup::new(region).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Warning: Could not create cleanup client: {}", e);
            return;
        }
    };
    let config = CleanupConfig {
        min_age: chrono::Duration::zero(),
        run_id: None,
        dry_run: false,
        force: true,
    };
    match cleanup.cleanup(&config).await {
        Ok(report) => {
            if report.deleted > 0 {
                eprintln!(
                    "Pre-test cleanup: deleted {} stale resources ({} failed)",
                    report.deleted, report.failed
                );
            }
        }
        Err(e) => {
            eprintln!("Warning: Pre-test cleanup failed: {}", e);
        }
    }
}

/// RAII guard that cleans up AWS resources when dropped (including on panic).
///
/// Tracks AWS resources created during a test and ensures they are cleaned up
/// in the correct order (instances -> SGs -> IAM -> S3) even if the test panics.
pub struct TestResourceTracker {
    region: String,
    pub bucket_names: Vec<String>,
    pub security_group_ids: Vec<String>,
    pub instance_ids: Vec<String>,
    pub iam_role_names: Vec<String>,
}

impl TestResourceTracker {
    pub fn new(region: &str) -> Self {
        Self {
            region: region.to_string(),
            bucket_names: Vec::new(),
            security_group_ids: Vec::new(),
            instance_ids: Vec::new(),
            iam_role_names: Vec::new(),
        }
    }

    pub fn track_bucket(&mut self, name: String) {
        self.bucket_names.push(name);
    }

    pub fn track_sg(&mut self, id: String) {
        self.security_group_ids.push(id);
    }

    pub fn track_instance(&mut self, id: String) {
        self.instance_ids.push(id);
    }

    pub fn track_iam_role(&mut self, name: String) {
        self.iam_role_names.push(name);
    }

    /// Run async cleanup. Called from Drop via tokio runtime.
    async fn cleanup_async(&self) {
        let ctx = AwsContext::new(&self.region).await;
        let ec2 = Ec2Client::from_context(&ctx);
        let s3 = S3Client::from_context(&ctx);
        let iam = IamClient::from_context(&ctx);

        // 1. Terminate instances first
        for instance_id in &self.instance_ids {
            eprintln!("  [cleanup] Terminating instance: {}", instance_id);
            if let Err(e) = ec2.terminate_instance(instance_id).await {
                eprintln!(
                    "  [cleanup] Warning: Failed to terminate {}: {}",
                    instance_id, e
                );
            }
        }

        // Wait for instances to terminate before deleting SGs
        for instance_id in &self.instance_ids {
            let _ = ec2.wait_for_terminated(instance_id).await;
        }

        // Small grace period for ENI release
        if !self.instance_ids.is_empty() {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        // 2. Delete security groups
        for sg_id in &self.security_group_ids {
            eprintln!("  [cleanup] Deleting security group: {}", sg_id);
            if let Err(e) = ec2.delete_security_group(sg_id).await {
                eprintln!("  [cleanup] Warning: Failed to delete SG {}: {}", sg_id, e);
            }
        }

        // 3. Delete IAM roles
        for role_name in &self.iam_role_names {
            eprintln!("  [cleanup] Deleting IAM role: {}", role_name);
            if let Err(e) = iam.delete_benchmark_role(role_name).await {
                eprintln!(
                    "  [cleanup] Warning: Failed to delete role {}: {}",
                    role_name, e
                );
            }
        }

        // 4. Delete S3 buckets
        for bucket_name in &self.bucket_names {
            eprintln!("  [cleanup] Deleting S3 bucket: {}", bucket_name);
            if let Err(e) = s3.delete_bucket(bucket_name).await {
                eprintln!(
                    "  [cleanup] Warning: Failed to delete bucket {}: {}",
                    bucket_name, e
                );
            }
        }
    }
}

impl Drop for TestResourceTracker {
    fn drop(&mut self) {
        if self.instance_ids.is_empty()
            && self.security_group_ids.is_empty()
            && self.iam_role_names.is_empty()
            && self.bucket_names.is_empty()
        {
            return;
        }

        eprintln!("[cleanup] TestResourceTracker dropping, cleaning up AWS resources...");

        // Use Handle::current() to run async cleanup within the existing tokio runtime
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // We're inside a tokio runtime - use block_in_place + block_on
            tokio::task::block_in_place(|| {
                handle.block_on(self.cleanup_async());
            });
        } else {
            // No runtime available - create a temporary one
            let rt = tokio::runtime::Runtime::new().expect("Failed to create cleanup runtime");
            rt.block_on(self.cleanup_async());
        }
    }
}
