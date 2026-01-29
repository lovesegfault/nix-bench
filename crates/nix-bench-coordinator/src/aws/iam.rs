//! IAM role and instance profile management for nix-bench agents

use super::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
use crate::aws::context::AwsContext;
use crate::wait::{WaitConfig, wait_for_resource};
use anyhow::{Context, Result};
use aws_sdk_iam::Client;
use aws_sdk_iam::error::ProvideErrorMetadata;
use chrono::Utc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

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

/// Build a single IAM tag, mapping the builder error to anyhow.
fn iam_tag(key: &str, value: &str) -> Result<aws_sdk_iam::types::Tag> {
    aws_sdk_iam::types::Tag::builder()
        .key(key)
        .value(value)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build IAM tag: {}", e))
}

/// Build the standard set of nix-bench IAM tags (tool, run_id, created_at, status).
fn standard_iam_tags(run_id: &str, created_at: &str) -> Result<Vec<aws_sdk_iam::types::Tag>> {
    Ok(vec![
        iam_tag(TAG_TOOL, TAG_TOOL_VALUE)?,
        iam_tag(TAG_RUN_ID, run_id)?,
        iam_tag(TAG_CREATED_AT, created_at)?,
        iam_tag(TAG_STATUS, tags::status::CREATING)?,
    ])
}

impl IamClient {
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
        // IAM role names have 64 char limit. "nix-bench-agent-" is 17 chars, leaving 47 for run_id.
        let max_run_id_len = 47;
        let truncated_run_id = if run_id.len() > max_run_id_len {
            &run_id[..max_run_id_len]
        } else {
            run_id
        };
        let role_name = format!("nix-bench-agent-{}", truncated_run_id);
        let profile_name = role_name.clone();
        let policy_name = "nix-bench-agent-policy";

        info!(role_name = %role_name, "Creating IAM role for benchmark");

        // Create the role with standard nix-bench tags
        let created_at = tags::format_created_at(Utc::now());
        let iam_tags = standard_iam_tags(run_id, &created_at)?;
        self.client
            .create_role()
            .role_name(&role_name)
            .assume_role_policy_document(EC2_ASSUME_ROLE_POLICY)
            .description(format!("nix-bench agent role for run {}", run_id))
            .set_tags(Some(iam_tags.clone()))
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
            .set_tags(Some(iam_tags))
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

        info!(role_name = %role_name, "Deleting IAM role and instance profile");

        self.try_iam(
            "remove role from instance profile",
            self.client
                .remove_role_from_instance_profile()
                .instance_profile_name(profile_name)
                .role_name(role_name)
                .send(),
        )
        .await;
        self.try_iam(
            "delete instance profile",
            self.client
                .delete_instance_profile()
                .instance_profile_name(profile_name)
                .send(),
        )
        .await;
        self.try_iam(
            "delete role policy",
            self.client
                .delete_role_policy()
                .role_name(role_name)
                .policy_name("nix-bench-agent-policy")
                .send(),
        )
        .await;
        self.try_iam(
            "detach SSM managed policy",
            self.client
                .detach_role_policy()
                .role_name(role_name)
                .policy_arn("arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore")
                .send(),
        )
        .await;
        self.try_iam(
            "delete role",
            self.client.delete_role().role_name(role_name).send(),
        )
        .await;

        info!(role_name = %role_name, "IAM role cleanup complete");
        Ok(())
    }

    /// Execute an IAM operation, logging warnings on failure.
    async fn try_iam<T, E: std::fmt::Debug>(
        &self,
        label: &str,
        fut: impl std::future::Future<Output = Result<T, E>>,
    ) {
        if let Err(e) = fut.await {
            debug!(error = ?e, "Failed to {label} (may already be done)");
        }
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

    /// Delete an orphaned instance profile (one that exists without its role)
    ///
    /// This handles the case where an instance profile was discovered without
    /// a paired role (e.g., role was deleted but profile deletion failed).
    pub async fn delete_instance_profile(&self, profile_name: &str) -> Result<()> {
        info!(profile_name = %profile_name, "Deleting orphaned instance profile");

        // First, try to remove any roles from the profile
        // This handles edge cases where a role might still be attached
        if let Ok(resp) = self
            .client
            .get_instance_profile()
            .instance_profile_name(profile_name)
            .send()
            .await
        {
            if let Some(profile) = resp.instance_profile() {
                for role in profile.roles() {
                    let role_name = role.role_name();
                    debug!(role_name = %role_name, profile_name = %profile_name, "Removing role from profile");
                    let _ = self
                        .client
                        .remove_role_from_instance_profile()
                        .instance_profile_name(profile_name)
                        .role_name(role_name)
                        .send()
                        .await;
                }
            }
        }

        // Delete the instance profile
        match self
            .client
            .delete_instance_profile()
            .instance_profile_name(profile_name)
            .send()
            .await
        {
            Ok(_) => {
                info!(profile_name = %profile_name, "Instance profile deleted");
                Ok(())
            }
            Err(e) => {
                // Check if it's a "not found" error (already deleted)
                let err_code = e.code().unwrap_or_default();
                if err_code == "NoSuchEntity" {
                    debug!(profile_name = %profile_name, "Instance profile already deleted");
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("Failed to delete instance profile: {}", e))
                }
            }
        }
    }
}
