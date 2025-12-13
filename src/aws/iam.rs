//! IAM role and instance profile management for nix-bench agents

use anyhow::{Context, Result};
use aws_sdk_iam::Client;
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
fn generate_agent_policy(bucket_name: &str) -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "S3Access",
                "Effect": "Allow",
                "Action": ["s3:GetObject"],
                "Resource": format!("arn:aws:s3:::{}/*", bucket_name)
            },
            {
                "Sid": "CloudWatchMetrics",
                "Effect": "Allow",
                "Action": ["cloudwatch:PutMetricData"],
                "Resource": "*",
                "Condition": {
                    "StringEquals": {
                        "cloudwatch:namespace": "NixBench"
                    }
                }
            },
            {
                "Sid": "CloudWatchLogs",
                "Effect": "Allow",
                "Action": [
                    "logs:CreateLogGroup",
                    "logs:CreateLogStream",
                    "logs:PutLogEvents",
                    "logs:DescribeLogStreams"
                ],
                "Resource": "arn:aws:logs:*:*:log-group:/nix-bench/*"
            }
        ]
    })
    .to_string()
}

impl IamClient {
    /// Create a new IAM client
    pub async fn new(region: &str) -> Result<Self> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.to_string()))
            .load()
            .await;

        let client = Client::new(&config);

        Ok(Self { client })
    }

    /// Create a role and instance profile for a benchmark run
    ///
    /// Returns (role_name, instance_profile_name)
    pub async fn create_benchmark_role(
        &self,
        run_id: &str,
        bucket_name: &str,
    ) -> Result<(String, String)> {
        let role_name = format!("nix-bench-agent-{}", &run_id[..13]); // Truncate for IAM limits
        let profile_name = role_name.clone();
        let policy_name = "nix-bench-agent-policy";

        info!(role_name = %role_name, "Creating IAM role for benchmark");

        // Create the role
        self.client
            .create_role()
            .role_name(&role_name)
            .assume_role_policy_document(EC2_ASSUME_ROLE_POLICY)
            .description(format!("nix-bench agent role for run {}", run_id))
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key("nix-bench:run-id")
                    .value(run_id)
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

        // Create instance profile
        self.client
            .create_instance_profile()
            .instance_profile_name(&profile_name)
            .tags(
                aws_sdk_iam::types::Tag::builder()
                    .key("nix-bench:run-id")
                    .value(run_id)
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

        // Wait a bit for IAM propagation
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

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
