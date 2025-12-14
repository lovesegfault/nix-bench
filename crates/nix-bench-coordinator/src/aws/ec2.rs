//! EC2 instance management

use crate::aws::context::AwsContext;
use crate::aws::error::{classify_anyhow_error, AwsError};
use anyhow::{Context, Result};
use aws_sdk_ec2::{
    types::{
        Filter, InstanceStateName, InstanceType, IpPermission, IpRange, ResourceType, Tag,
        TagSpecification,
    },
    Client,
};
use backon::{ExponentialBuilder, Retryable};
use chrono::Utc;
use nix_bench_common::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
use std::time::Duration;
use tracing::{debug, info, warn};

/// EC2 client for managing benchmark instances
pub struct Ec2Client {
    client: Client,
}

/// Launched instance info
#[derive(Debug, Clone)]
pub struct LaunchedInstance {
    pub instance_id: String,
    pub instance_type: String,
    pub system: String,
    pub public_ip: Option<String>,
}

/// Configuration for launching an EC2 instance
#[derive(Debug, Clone)]
pub struct LaunchInstanceConfig {
    /// Unique run identifier for tagging
    pub run_id: String,
    /// EC2 instance type (e.g., "c7i.xlarge")
    pub instance_type: String,
    /// System architecture ("x86_64-linux" or "aarch64-linux")
    pub system: String,
    /// User data script (will be base64 encoded)
    pub user_data: String,
    /// Optional VPC subnet ID
    pub subnet_id: Option<String>,
    /// Optional security group ID
    pub security_group_id: Option<String>,
    /// Optional IAM instance profile name
    pub iam_instance_profile: Option<String>,
}

impl LaunchInstanceConfig {
    /// Create a new launch configuration with required fields
    pub fn new(run_id: impl Into<String>, instance_type: impl Into<String>, system: impl Into<String>, user_data: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            instance_type: instance_type.into(),
            system: system.into(),
            user_data: user_data.into(),
            subnet_id: None,
            security_group_id: None,
            iam_instance_profile: None,
        }
    }

    /// Set the VPC subnet ID
    pub fn with_subnet(mut self, subnet_id: impl Into<String>) -> Self {
        self.subnet_id = Some(subnet_id.into());
        self
    }

    /// Set the security group ID
    pub fn with_security_group(mut self, security_group_id: impl Into<String>) -> Self {
        self.security_group_id = Some(security_group_id.into());
        self
    }

    /// Set the IAM instance profile name
    pub fn with_iam_profile(mut self, profile_name: impl Into<String>) -> Self {
        self.iam_instance_profile = Some(profile_name.into());
        self
    }
}

impl Ec2Client {
    /// Create a new EC2 client (loads AWS config from environment)
    pub async fn new(region: &str) -> Result<Self> {
        let ctx = AwsContext::new(region).await;
        Ok(Self::from_context(&ctx))
    }

    /// Create an EC2 client from a pre-loaded AWS context
    pub fn from_context(ctx: &AwsContext) -> Self {
        Self {
            client: ctx.ec2_client(),
        }
    }

    /// Get the latest AL2023 AMI for the given architecture
    pub async fn get_al2023_ami(&self, arch: &str) -> Result<String> {
        let arch_filter = if arch == "aarch64-linux" {
            "arm64"
        } else {
            "x86_64"
        };

        let response = self
            .client
            .describe_images()
            .owners("amazon")
            .filters(
                Filter::builder()
                    .name("name")
                    .values(format!("al2023-ami-*-{}", arch_filter))
                    .build(),
            )
            .filters(Filter::builder().name("state").values("available").build())
            .filters(
                Filter::builder()
                    .name("architecture")
                    .values(arch_filter)
                    .build(),
            )
            .send()
            .await
            .context("Failed to describe images")?;

        let images = response.images();

        // Sort by creation date and get the latest
        let mut images: Vec<_> = images.iter().collect();
        images.sort_by(|a, b| {
            b.creation_date()
                .unwrap_or_default()
                .cmp(a.creation_date().unwrap_or_default())
        });

        let ami = images
            .first()
            .and_then(|img| img.image_id())
            .context("No AL2023 AMI found")?;

        debug!(ami = %ami, arch = %arch, "Found AL2023 AMI");

        Ok(ami.to_string())
    }

    /// Launch an EC2 instance with the given configuration
    ///
    /// When an IAM instance profile is provided, this function will retry the launch
    /// if EC2 rejects the profile due to IAM eventual consistency (the profile may
    /// not be visible to EC2 immediately after creation).
    pub async fn launch_instance(&self, config: LaunchInstanceConfig) -> Result<LaunchedInstance> {
        let ami_id = self.get_al2023_ami(&config.system).await?;

        let instance_type_enum: InstanceType = config
            .instance_type
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid instance type: {}", config.instance_type))?;

        info!(
            instance_type = %config.instance_type,
            system = %config.system,
            ami = %ami_id,
            "Launching instance"
        );

        let user_data_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            config.user_data.as_bytes(),
        );

        // Only use retry logic if an IAM profile is provided (IAM eventual consistency)
        if let Some(ref profile) = config.iam_instance_profile {
            let profile = profile.clone();
            (|| async {
                self.do_launch_instance(
                    &ami_id,
                    instance_type_enum.clone(),
                    &config.run_id,
                    &config.instance_type,
                    &config.system,
                    &user_data_b64,
                    config.subnet_id.as_deref(),
                    config.security_group_id.as_deref(),
                    Some(&profile),
                )
                .await
            })
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_secs(1))
                    .with_max_delay(Duration::from_secs(10))
                    .with_max_times(10),
            )
            .when(|e| {
                // Only retry on IAM propagation errors
                matches!(classify_anyhow_error(e), AwsError::IamPropagationDelay)
            })
            .notify(|e, dur| {
                warn!(
                    delay = ?dur,
                    profile = %profile,
                    error = %e,
                    "IAM instance profile not yet visible to EC2, retrying..."
                );
            })
            .await
        } else {
            // No IAM profile, no need for retry logic
            self.do_launch_instance(
                &ami_id,
                instance_type_enum,
                &config.run_id,
                &config.instance_type,
                &config.system,
                &user_data_b64,
                config.subnet_id.as_deref(),
                config.security_group_id.as_deref(),
                None,
            )
            .await
        }
    }

    /// Internal method to perform the actual RunInstances call
    async fn do_launch_instance(
        &self,
        ami_id: &str,
        instance_type_enum: InstanceType,
        run_id: &str,
        instance_type: &str,
        system: &str,
        user_data_b64: &str,
        subnet_id: Option<&str>,
        security_group_id: Option<&str>,
        iam_instance_profile: Option<&str>,
    ) -> Result<LaunchedInstance> {
        let created_at = tags::format_created_at(Utc::now());
        let mut request = self
            .client
            .run_instances()
            .image_id(ami_id)
            .instance_type(instance_type_enum)
            .min_count(1)
            .max_count(1)
            .user_data(user_data_b64)
            .tag_specifications(
                TagSpecification::builder()
                    .resource_type(ResourceType::Instance)
                    .tags(Tag::builder().key(TAG_TOOL).value(TAG_TOOL_VALUE).build())
                    .tags(Tag::builder().key(TAG_RUN_ID).value(run_id).build())
                    .tags(Tag::builder().key(TAG_CREATED_AT).value(&created_at).build())
                    .tags(Tag::builder().key(TAG_STATUS).value(tags::status::CREATING).build())
                    .tags(
                        Tag::builder()
                            .key("Name")
                            .value(format!("nix-bench-{}-{}", run_id, instance_type))
                            .build(),
                    )
                    .tags(
                        Tag::builder()
                            .key(tags::TAG_INSTANCE_TYPE)
                            .value(instance_type)
                            .build(),
                    )
                    .build(),
            );

        if let Some(subnet) = subnet_id {
            request = request.subnet_id(subnet);
        }

        if let Some(sg) = security_group_id {
            request = request.security_group_ids(sg);
        }

        if let Some(profile) = iam_instance_profile {
            request = request.iam_instance_profile(
                aws_sdk_ec2::types::IamInstanceProfileSpecification::builder()
                    .name(profile)
                    .build(),
            );
        }

        let response = request.send().await.context("Failed to launch instance")?;

        let instance = response
            .instances()
            .first()
            .context("No instance returned")?;

        let instance_id = instance
            .instance_id()
            .context("No instance ID")?
            .to_string();

        info!(instance_id = %instance_id, "Instance launched");

        Ok(LaunchedInstance {
            instance_id,
            instance_type: instance_type.to_string(),
            system: system.to_string(),
            public_ip: None,
        })
    }

    /// Default timeout for waiting for instance to be running (10 minutes)
    const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 600;

    /// Wait for an instance to be running and get its public IP
    ///
    /// # Arguments
    /// * `instance_id` - The instance ID to wait for
    /// * `timeout_secs` - Optional timeout in seconds (default: 600 = 10 minutes)
    pub async fn wait_for_running(
        &self,
        instance_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<String>> {
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(Self::DEFAULT_WAIT_TIMEOUT_SECS));
        info!(
            instance_id = %instance_id,
            timeout_secs = timeout.as_secs(),
            "Waiting for instance to be running"
        );

        let result = tokio::time::timeout(timeout, self.wait_for_running_inner(instance_id)).await;

        match result {
            Ok(inner_result) => inner_result,
            Err(_) => {
                warn!(
                    instance_id = %instance_id,
                    timeout_secs = timeout.as_secs(),
                    "Timed out waiting for instance to be running"
                );
                Err(anyhow::anyhow!(
                    "Timeout waiting for instance {} to be running after {}s",
                    instance_id,
                    timeout.as_secs()
                ))
            }
        }
    }

    /// Inner wait loop without timeout (used by wait_for_running)
    async fn wait_for_running_inner(&self, instance_id: &str) -> Result<Option<String>> {
        loop {
            let response = self
                .client
                .describe_instances()
                .instance_ids(instance_id)
                .send()
                .await
                .context("Failed to describe instance")?;

            let instance = response
                .reservations()
                .first()
                .and_then(|r| r.instances().first())
                .context("Instance not found")?;

            let state = instance
                .state()
                .and_then(|s| s.name())
                .unwrap_or(&InstanceStateName::Pending);

            match state {
                InstanceStateName::Running => {
                    let public_ip = instance.public_ip_address().map(|s| s.to_string());
                    info!(instance_id = %instance_id, public_ip = ?public_ip, "Instance is running");
                    return Ok(public_ip);
                }
                InstanceStateName::Pending => {
                    debug!(instance_id = %instance_id, "Instance still pending");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                _ => {
                    anyhow::bail!(
                        "Instance {} entered unexpected state: {:?}",
                        instance_id,
                        state
                    );
                }
            }
        }
    }

    /// Terminate an instance
    pub async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        info!(instance_id = %instance_id, "Terminating instance");

        self.client
            .terminate_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .context("Failed to terminate instance")?;

        Ok(())
    }

    /// Wait for an instance to be fully terminated
    ///
    /// This is needed before deleting security groups, as they can't be deleted
    /// while instances are still using them.
    pub async fn wait_for_terminated(&self, instance_id: &str) -> Result<()> {
        const MAX_WAIT_SECS: u64 = 300; // 5 minutes max
        let start = std::time::Instant::now();

        loop {
            if start.elapsed().as_secs() > MAX_WAIT_SECS {
                warn!(instance_id = %instance_id, "Timeout waiting for instance to terminate");
                return Ok(()); // Don't fail, just continue
            }

            let response = self
                .client
                .describe_instances()
                .instance_ids(instance_id)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let state = resp
                        .reservations()
                        .first()
                        .and_then(|r| r.instances().first())
                        .and_then(|i| i.state())
                        .and_then(|s| s.name());

                    match state {
                        Some(InstanceStateName::Terminated) => {
                            debug!(instance_id = %instance_id, "Instance terminated");
                            return Ok(());
                        }
                        Some(InstanceStateName::ShuttingDown) => {
                            debug!(instance_id = %instance_id, "Instance still shutting down");
                        }
                        Some(other) => {
                            debug!(instance_id = %instance_id, state = ?other, "Unexpected state while waiting for termination");
                        }
                        None => {
                            // Instance might not exist anymore
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    // Use centralized error classification
                    let err = anyhow::Error::from(e);
                    if classify_anyhow_error(&err).is_not_found() {
                        // Instance already gone
                        return Ok(());
                    }
                    warn!(instance_id = %instance_id, error = ?err, "Error checking instance state");
                }
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    /// Get console output from an instance
    pub async fn get_console_output(&self, instance_id: &str) -> Result<Option<String>> {
        let response = self
            .client
            .get_console_output()
            .instance_id(instance_id)
            .send()
            .await
            .context("Failed to get console output")?;

        // Console output is base64 encoded
        if let Some(encoded) = response.output() {
            use base64::Engine;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok());
            Ok(decoded)
        } else {
            Ok(None)
        }
    }

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
        let sg_name = format!("nix-bench-{}", &run_id[..13.min(run_id.len())]);
        info!(name = %sg_name, "Creating security group");

        // Get VPC ID if not provided
        let vpc_id = match vpc_id {
            Some(id) => id.to_string(),
            None => {
                // Get default VPC
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
                    .tags(Tag::builder().key(TAG_CREATED_AT).value(&created_at).build())
                    .tags(Tag::builder().key(TAG_STATUS).value(tags::status::CREATING).build())
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

        // Add SSH ingress rule (port 22 from anywhere - instances may need SSH access)
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
    /// # Arguments
    /// * `security_group_id` - The security group ID to delete
    pub async fn delete_security_group(&self, security_group_id: &str) -> Result<()> {
        info!(sg_id = %security_group_id, "Deleting security group");

        self.client
            .delete_security_group()
            .group_id(security_group_id)
            .send()
            .await
            .context("Failed to delete security group")?;

        info!(sg_id = %security_group_id, "Security group deleted");

        Ok(())
    }

    /// Add an ingress rule to a security group for gRPC traffic (port 50051)
    ///
    /// # Arguments
    /// * `security_group_id` - The security group ID to modify
    /// * `cidr_ip` - The CIDR IP to allow (e.g., "1.2.3.4/32")
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

        debug!(
            security_group_id = %security_group_id,
            "Successfully added gRPC ingress rule"
        );

        Ok(())
    }

    /// Remove an ingress rule from a security group for gRPC traffic (port 50051)
    ///
    /// # Arguments
    /// * `security_group_id` - The security group ID to modify
    /// * `cidr_ip` - The CIDR IP to remove (e.g., "1.2.3.4/32")
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

        self.client
            .revoke_security_group_ingress()
            .group_id(security_group_id)
            .ip_permissions(ip_permission)
            .send()
            .await
            .context("Failed to remove gRPC ingress rule")?;

        debug!(
            security_group_id = %security_group_id,
            "Successfully removed gRPC ingress rule"
        );

        Ok(())
    }
}

/// Get the public IP address of the coordinator (this machine)
///
/// Uses AWS checkip service to get the public IP.
pub async fn get_coordinator_public_ip() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")?;

    let response = client
        .get("https://checkip.amazonaws.com")
        .send()
        .await
        .context("Failed to fetch public IP from checkip.amazonaws.com")?;

    let ip = response
        .text()
        .await
        .context("Failed to read response body")?
        .trim()
        .to_string();

    // Validate it looks like an IP address
    if ip.parse::<std::net::IpAddr>().is_err() {
        anyhow::bail!("Invalid IP address received: {}", ip);
    }

    debug!(public_ip = %ip, "Detected coordinator public IP");
    Ok(ip)
}

/// Trait for EC2 operations that can be mocked in tests.
///
/// This trait abstracts the EC2 client operations to enable unit testing
/// of orchestration logic without hitting real AWS.
///
/// Note: Some parameters use `Option<String>` instead of `Option<&str>` to work
/// around mockall lifetime limitations.
#[allow(async_fn_in_trait)] // Internal use only, Send+Sync bounds on trait are sufficient
#[cfg_attr(test, mockall::automock)]
pub trait Ec2Operations: Send + Sync {
    /// Launch an EC2 instance with the given configuration
    async fn launch_instance(&self, config: LaunchInstanceConfig) -> Result<LaunchedInstance>;

    /// Wait for instance to be running and get its public IP
    async fn wait_for_running(
        &self,
        instance_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<String>>;

    /// Terminate an instance
    async fn terminate_instance(&self, instance_id: &str) -> Result<()>;

    /// Wait for instance to be fully terminated
    async fn wait_for_terminated(&self, instance_id: &str) -> Result<()>;

    /// Get console output from an instance
    async fn get_console_output(&self, instance_id: &str) -> Result<Option<String>>;

    /// Create a security group
    async fn create_security_group(
        &self,
        run_id: &str,
        coordinator_cidr: &str,
        vpc_id: Option<String>,
    ) -> Result<String>;

    /// Delete a security group
    async fn delete_security_group(&self, security_group_id: &str) -> Result<()>;

    /// Add gRPC ingress rule to security group
    async fn add_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()>;

    /// Remove gRPC ingress rule from security group
    async fn remove_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()>;
}

impl Ec2Operations for Ec2Client {
    async fn launch_instance(&self, config: LaunchInstanceConfig) -> Result<LaunchedInstance> {
        Ec2Client::launch_instance(self, config).await
    }

    async fn wait_for_running(
        &self,
        instance_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<String>> {
        Ec2Client::wait_for_running(self, instance_id, timeout_secs).await
    }

    async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        Ec2Client::terminate_instance(self, instance_id).await
    }

    async fn wait_for_terminated(&self, instance_id: &str) -> Result<()> {
        Ec2Client::wait_for_terminated(self, instance_id).await
    }

    async fn get_console_output(&self, instance_id: &str) -> Result<Option<String>> {
        Ec2Client::get_console_output(self, instance_id).await
    }

    async fn create_security_group(
        &self,
        run_id: &str,
        coordinator_cidr: &str,
        vpc_id: Option<String>,
    ) -> Result<String> {
        Ec2Client::create_security_group(self, run_id, coordinator_cidr, vpc_id.as_deref()).await
    }

    async fn delete_security_group(&self, security_group_id: &str) -> Result<()> {
        Ec2Client::delete_security_group(self, security_group_id).await
    }

    async fn add_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()> {
        Ec2Client::add_grpc_ingress_rule(self, security_group_id, cidr_ip).await
    }

    async fn remove_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()> {
        Ec2Client::remove_grpc_ingress_rule(self, security_group_id, cidr_ip).await
    }
}
