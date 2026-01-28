//! Security group management

use super::Ec2Client;
use crate::aws::error::{classify_anyhow_error, classify_aws_error};
use crate::aws::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
use anyhow::{Context, Result};
use aws_sdk_ec2::error::ProvideErrorMetadata;
use aws_sdk_ec2::types::{Filter, IpPermission, IpRange, ResourceType, Tag, TagSpecification};
use backon::{ExponentialBuilder, Retryable};
use chrono::Utc;
use std::time::Duration;
use tracing::{debug, info, warn};

impl Ec2Client {
    /// Create a security group for nix-bench instances with SSH and gRPC access
    ///
    /// # Arguments
    /// * `run_id` - The run ID for naming and tagging
    /// * `coordinator_cidr` - CIDR for coordinator gRPC access (e.g., "1.2.3.4/32")
    /// * `vpc_id` - Optional VPC ID (uses default VPC if not specified)
    ///
    /// # Returns
    /// The security group ID
    pub async fn create_security_group(
        &self,
        run_id: &str,
        coordinator_cidr: &str,
        vpc_id: Option<&str>,
    ) -> Result<String> {
        let sg_name = format!("nix-bench-{}", run_id);
        // AWS SG names can be up to 255 chars; truncate only if needed
        let sg_name = if sg_name.len() > 255 {
            sg_name[..255].to_string()
        } else {
            sg_name
        };
        info!(name = %sg_name, "Creating security group");

        // Get VPC ID if not provided
        let vpc_id = match vpc_id {
            Some(id) => id.to_string(),
            None => {
                let vpcs = self
                    .client
                    .describe_vpcs()
                    .filters(Filter::builder().name("isDefault").values("true").build())
                    .send()
                    .await
                    .context("Failed to describe VPCs")?;

                vpcs.vpcs()
                    .first()
                    .and_then(|v| v.vpc_id())
                    .context("No default VPC found")?
                    .to_string()
            }
        };

        // Create the security group
        let created_at = tags::format_created_at(Utc::now());
        let create_response = self
            .client
            .create_security_group()
            .group_name(&sg_name)
            .description(format!("nix-bench security group for run {}", run_id))
            .vpc_id(&vpc_id)
            .tag_specifications(
                TagSpecification::builder()
                    .resource_type(ResourceType::SecurityGroup)
                    .tags(Tag::builder().key(TAG_TOOL).value(TAG_TOOL_VALUE).build())
                    .tags(Tag::builder().key(TAG_RUN_ID).value(run_id).build())
                    .tags(
                        Tag::builder()
                            .key(TAG_CREATED_AT)
                            .value(&created_at)
                            .build(),
                    )
                    .tags(
                        Tag::builder()
                            .key(TAG_STATUS)
                            .value(tags::status::CREATING)
                            .build(),
                    )
                    .tags(Tag::builder().key("Name").value(&sg_name).build())
                    .build(),
            )
            .send()
            .await
            .context("Failed to create security group")?;

        let sg_id = create_response
            .group_id()
            .context("No security group ID in response")?
            .to_string();

        info!(sg_id = %sg_id, "Created security group, adding rules");

        // Add SSH ingress rule (port 22 from anywhere)
        let ssh_permission = IpPermission::builder()
            .ip_protocol("tcp")
            .from_port(22)
            .to_port(22)
            .ip_ranges(
                IpRange::builder()
                    .cidr_ip("0.0.0.0/0")
                    .description("SSH access")
                    .build(),
            )
            .build();

        // Add gRPC ingress rule (port 50051 from coordinator only)
        let grpc_permission = IpPermission::builder()
            .ip_protocol("tcp")
            .from_port(50051)
            .to_port(50051)
            .ip_ranges(
                IpRange::builder()
                    .cidr_ip(coordinator_cidr)
                    .description("nix-bench gRPC coordinator access")
                    .build(),
            )
            .build();

        self.client
            .authorize_security_group_ingress()
            .group_id(&sg_id)
            .ip_permissions(ssh_permission)
            .ip_permissions(grpc_permission)
            .send()
            .await
            .context("Failed to add ingress rules to security group")?;

        info!(sg_id = %sg_id, "Security group created with SSH and gRPC rules");

        Ok(sg_id)
    }

    /// Delete a security group
    ///
    /// Returns Ok(()) if the security group was deleted or if it doesn't exist (idempotent for cleanup).
    /// Retries on DependencyViolation errors (e.g., when ENIs are still releasing after instance termination).
    pub async fn delete_security_group(&self, security_group_id: &str) -> Result<()> {
        info!(sg_id = %security_group_id, "Deleting security group");

        let sg_id = security_group_id.to_string();
        let sg_id_for_log = sg_id.clone();

        (|| async {
            match self
                .client
                .delete_security_group()
                .group_id(&sg_id)
                .send()
                .await
            {
                Ok(_) => {
                    info!(sg_id = %sg_id, "Security group deleted");
                    Ok(())
                }
                Err(sdk_error) => {
                    let aws_error = classify_aws_error(sdk_error.code(), sdk_error.message());
                    if aws_error.is_not_found() {
                        debug!(sg_id = %sg_id, "Security group already deleted or doesn't exist");
                        Ok(())
                    } else {
                        Err(anyhow::Error::from(sdk_error)
                            .context("Failed to delete security group"))
                    }
                }
            }
        })
        .retry(
            ExponentialBuilder::default()
                .with_min_delay(Duration::from_secs(10))
                .with_max_delay(Duration::from_secs(60))
                .with_max_times(5),
        )
        .when(|e| classify_anyhow_error(e).is_retryable())
        .notify(|e, dur| {
            warn!(
                sg_id = %sg_id_for_log,
                delay = ?dur,
                error = %e,
                "Security group deletion failed, retrying..."
            );
        })
        .await
    }

    /// Add an ingress rule to a security group for gRPC traffic (port 50051)
    pub async fn add_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()> {
        info!(
            security_group_id = %security_group_id,
            cidr_ip = %cidr_ip,
            "Adding gRPC ingress rule (port 50051)"
        );

        let ip_permission = IpPermission::builder()
            .ip_protocol("tcp")
            .from_port(50051)
            .to_port(50051)
            .ip_ranges(
                IpRange::builder()
                    .cidr_ip(cidr_ip)
                    .description("nix-bench gRPC coordinator access")
                    .build(),
            )
            .build();

        self.client
            .authorize_security_group_ingress()
            .group_id(security_group_id)
            .ip_permissions(ip_permission)
            .send()
            .await
            .context("Failed to add gRPC ingress rule")?;

        debug!(security_group_id = %security_group_id, "Successfully added gRPC ingress rule");

        Ok(())
    }

    /// Remove an ingress rule from a security group for gRPC traffic (port 50051)
    ///
    /// Returns Ok(()) if the rule was removed or if it doesn't exist (idempotent for cleanup).
    pub async fn remove_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()> {
        info!(
            security_group_id = %security_group_id,
            cidr_ip = %cidr_ip,
            "Removing gRPC ingress rule (port 50051)"
        );

        let ip_permission = IpPermission::builder()
            .ip_protocol("tcp")
            .from_port(50051)
            .to_port(50051)
            .ip_ranges(IpRange::builder().cidr_ip(cidr_ip).build())
            .build();

        match self
            .client
            .revoke_security_group_ingress()
            .group_id(security_group_id)
            .ip_permissions(ip_permission)
            .send()
            .await
        {
            Ok(_) => {
                debug!(security_group_id = %security_group_id, "Successfully removed gRPC ingress rule");
                Ok(())
            }
            Err(sdk_error) => {
                if classify_aws_error(sdk_error.code(), sdk_error.message()).is_not_found() {
                    debug!(
                        security_group_id = %security_group_id,
                        cidr_ip = %cidr_ip,
                        "Security group rule already removed or doesn't exist"
                    );
                    Ok(())
                } else {
                    Err(anyhow::Error::from(sdk_error)
                        .context("Failed to remove gRPC ingress rule"))
                }
            }
        }
    }
}
