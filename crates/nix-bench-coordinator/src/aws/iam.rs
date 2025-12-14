//! IAM role and instance profile management for nix-bench agents

use crate::aws::context::AwsContext;
use crate::wait::{wait_for_resource, WaitConfig};
use anyhow::{Context, Result};
use aws_sdk_iam::Client;
use chrono::Utc;
use nix_bench_common::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// IAM client for managing roles and instance profiles
pub struct IamClient {
    client: Client,
}

/// The trust policy allowing EC2 to assume the role
const EC2_ASSUME_ROLE_POLICY: &str = r#"{
    "Version": "2012-10-17",
    "Statement": [
        {
            "Effect": "Allow",
            "Principal": {
                "Service": "ec2.amazonaws.com"
            },
            "Action": "sts:AssumeRole"
        }
    ]
}"#;

/// Generate the inline policy for nix-bench agent
///
/// The agent needs:
/// - S3 read access to download config
/// - S3 write access to upload results
fn generate_agent_policy(bucket_name: &str) -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "S3ReadAccess",
                "Effect": "Allow",
                "Action": ["s3:GetObject"],
                "Resource": format!("arn:aws:s3:::{}/*", bucket_name)
            },
            {
                "Sid": "S3WriteAccess",
                "Effect": "Allow",
                "Action": ["s3:PutObject"],
                "Resource": format!("arn:aws:s3:::{}/*", bucket_name)
            }
        ]
    })
    .to_string()
}

impl IamClient {
    /// Create a new IAM client
    pub async fn new(region: &str) -> Result<Self> {
        let ctx = AwsContext::new(region).await;
        Ok(Self::from_context(&ctx))
    }

    /// Create an IAM client from a pre-loaded AWS context
    pub fn from_context(ctx: &AwsContext) -> Self {
        Self {
            client: ctx.iam_client(),
        }
    }

    /// Create a role and instance profile for a benchmark run
    ///
    /// Returns (role_name, instance_profile_name)
    ///
    /// The optional `cancel` token allows cancelling the wait for IAM propagation.
    pub async fn create_benchmark_role(
        &self,
        run_id: &str,
        bucket_name: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<(String, String)> {
        let role_name = format!("nix-bench-agent-{}", &run_id[..13]); // Truncate for IAM limits
        let profile_name = role_name.clone();
        let policy_name = "nix-bench-agent-policy";

        info!(role_name = %role_name, "Creating IAM role for benchmark");

        // Create the role with standard nix-bench tags
        let created_at = tags::format_created_at(Utc::now());
        self.client
            .create_role()
            .role_name(&role_name)
            .assume_role_policy_document(EC2_ASSUME_ROLE_POLICY)
            .description(format!("nix-bench agent role for run {}", run_id))
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_TOOL)
                    .value(TAG_TOOL_VALUE)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_RUN_ID)
                    .value(run_id)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_CREATED_AT)
                    .value(&created_at)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_STATUS)
                    .value(tags::status::CREATING)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .send()
            .await
            .context("Failed to create IAM role")?;

        debug!(role_name = %role_name, "IAM role created");

        // Attach inline policy
        let policy_document = generate_agent_policy(bucket_name);
        self.client
            .put_role_policy()
            .role_name(&role_name)
            .policy_name(policy_name)
            .policy_document(&policy_document)
            .send()
            .await
            .context("Failed to attach inline policy to role")?;

        debug!(role_name = %role_name, "Inline policy attached");

        // Attach SSM managed policy for Session Manager access
        self.client
            .attach_role_policy()
            .role_name(&role_name)
            .policy_arn("arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore")
            .send()
            .await
            .context("Failed to attach SSM managed policy")?;

        debug!(role_name = %role_name, "SSM managed policy attached");

        // Create instance profile with same tags as role
        self.client
            .create_instance_profile()
            .instance_profile_name(&profile_name)
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_TOOL)
                    .value(TAG_TOOL_VALUE)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_RUN_ID)
                    .value(run_id)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_CREATED_AT)
                    .value(&created_at)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key(TAG_STATUS)
                    .value(tags::status::CREATING)
                    .build()
                    .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))?,
            )
            .send()
            .await
            .context("Failed to create instance profile")?;

        debug!(profile_name = %profile_name, "Instance profile created");

        // Add role to instance profile
        self.client
            .add_role_to_instance_profile()
            .instance_profile_name(&profile_name)
            .role_name(&role_name)
            .send()
            .await
            .context("Failed to add role to instance profile")?;

        info!(
            role_name = %role_name,
            profile_name = %profile_name,
            "IAM role and instance profile created"
        );

        // Wait for instance profile to be ready (role attached) with exponential backoff
        // This is more responsive than a fixed 10-second sleep and can be cancelled.
        let client = self.client.clone();
        let profile_name_clone = profile_name.clone();

        wait_for_resource(
            WaitConfig {
                initial_delay: Duration::from_millis(500),
                max_delay: Duration::from_secs(5),
                timeout: Duration::from_secs(60), // Increased timeout for IAM propagation
                jitter: 0.25,
            },
            cancel,
            || {
                let c = client.clone();
                let p = profile_name_clone.clone();
                async move {
                    match c
                        .get_instance_profile()
                        .instance_profile_name(&p)
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            // Check if role is attached to the profile
                            let has_role = resp
                                .instance_profile()
                                .map(|profile| !profile.roles().is_empty())
                                .unwrap_or(false);
                            Ok(has_role)
                        }
                        Err(_) => Ok(false), // Profile not ready yet
                    }
                }
            },
            "IAM instance profile",
        )
        .await
        .context("Waiting for IAM instance profile to be ready")?;

        // Note: EC2 may still not recognize the profile due to eventual consistency.
        // The launch_instance function handles this with retry logic.
        debug!(profile_name = %profile_name, "IAM instance profile visible in IAM API");

        Ok((role_name, profile_name))
    }

    /// Delete a role and its instance profile
    pub async fn delete_benchmark_role(&self, role_name: &str) -> Result<()> {
        let profile_name = role_name; // They have the same name
        let policy_name = "nix-bench-agent-policy";

        info!(role_name = %role_name, "Deleting IAM role and instance profile");

        // Remove role from instance profile (ignore errors - might already be removed)
        if let Err(e) = self
            .client
            .remove_role_from_instance_profile()
            .instance_profile_name(profile_name)
            .role_name(role_name)
            .send()
            .await
        {
            debug!(error = ?e, "Failed to remove role from instance profile (may already be removed)");
        }

        // Delete instance profile
        if let Err(e) = self
            .client
            .delete_instance_profile()
            .instance_profile_name(profile_name)
            .send()
            .await
        {
            warn!(error = ?e, profile_name = %profile_name, "Failed to delete instance profile");
        } else {
            debug!(profile_name = %profile_name, "Instance profile deleted");
        }

        // Delete inline policy from role
        if let Err(e) = self
            .client
            .delete_role_policy()
            .role_name(role_name)
            .policy_name(policy_name)
            .send()
            .await
        {
            debug!(error = ?e, "Failed to delete role policy (may already be deleted)");
        }

        // Detach SSM managed policy
        if let Err(e) = self
            .client
            .detach_role_policy()
            .role_name(role_name)
            .policy_arn("arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore")
            .send()
            .await
        {
            debug!(error = ?e, "Failed to detach SSM managed policy (may already be detached)");
        }

        // Delete role
        if let Err(e) = self.client.delete_role().role_name(role_name).send().await {
            warn!(error = ?e, role_name = %role_name, "Failed to delete IAM role");
        } else {
            info!(role_name = %role_name, "IAM role deleted");
        }

        Ok(())
    }

    /// Check if an instance profile exists
    pub async fn instance_profile_exists(&self, profile_name: &str) -> bool {
        self.client
            .get_instance_profile()
            .instance_profile_name(profile_name)
            .send()
            .await
            .is_ok()
    }
}

/// Trait for IAM operations.
pub trait IamOperations: Send + Sync {
    /// Create a role and instance profile for a benchmark run.
    /// Pass `cancel.clone()` if you have a token reference.
    fn create_benchmark_role(
        &self,
        run_id: &str,
        bucket_name: &str,
        cancel: Option<CancellationToken>,
    ) -> impl Future<Output = Result<(String, String)>> + Send;

    /// Delete a role and its instance profile
    fn delete_benchmark_role(&self, role_name: &str) -> impl Future<Output = Result<()>> + Send;

    /// Check if an instance profile exists
    fn instance_profile_exists(&self, profile_name: &str) -> impl Future<Output = bool> + Send;
}

impl IamOperations for IamClient {
    async fn create_benchmark_role(
        &self,
        run_id: &str,
        bucket_name: &str,
        cancel: Option<CancellationToken>,
    ) -> Result<(String, String)> {
        IamClient::create_benchmark_role(self, run_id, bucket_name, cancel.as_ref()).await
    }

    async fn delete_benchmark_role(&self, role_name: &str) -> Result<()> {
        IamClient::delete_benchmark_role(self, role_name).await
    }

    async fn instance_profile_exists(&self, profile_name: &str) -> bool {
        IamClient::instance_profile_exists(self, profile_name).await
    }
}
