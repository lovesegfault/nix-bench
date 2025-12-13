//! EC2 instance management

use crate::aws::error::{classify_anyhow_error, AwsError};
use crate::aws_context::AwsContext;
use anyhow::{Context, Result};
use aws_sdk_ec2::{
    types::{
        Filter, InstanceStateName, InstanceType, IpPermission, IpRange, ResourceType, Tag,
        TagSpecification,
    },
    Client,
};
use backon::{ExponentialBuilder, Retryable};
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
    pub async fn launch_instance(
        &self,
        run_id: &str,
        instance_type: &str,
        system: &str,
        user_data: &str,
        subnet_id: Option<&str>,
        security_group_id: Option<&str>,
        iam_instance_profile: Option<&str>,
    ) -> Result<LaunchedInstance> {
        let ami_id = self.get_al2023_ami(system).await?;

        let instance_type_enum: InstanceType = instance_type
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid instance type: {}", instance_type))?;

        info!(
            instance_type = %instance_type,
            system = %system,
            ami = %ami_id,
            "Launching instance"
        );

        let user_data_b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            user_data.as_bytes(),
        );

        // Only use retry logic if an IAM profile is provided (IAM eventual consistency)
        if iam_instance_profile.is_some() {
            let profile = iam_instance_profile.unwrap();
            (|| async {
                self.do_launch_instance(
                    &ami_id,
                    instance_type_enum.clone(),
                    run_id,
                    instance_type,
                    system,
                    &user_data_b64,
                    subnet_id,
                    security_group_id,
                    Some(profile),
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
                    profile = profile,
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
                run_id,
                instance_type,
                system,
                &user_data_b64,
                subnet_id,
                security_group_id,
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
                    .tags(
                        Tag::builder()
                            .key("Name")
                            .value(format!("nix-bench-{}-{}", run_id, instance_type))
                            .build(),
                    )
                    .tags(Tag::builder().key("nix-bench:run-id").value(run_id).build())
                    .tags(
                        Tag::builder()
                            .key("nix-bench:instance-type")
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
        let create_response = self
            .client
            .create_security_group()
            .group_name(&sg_name)
            .description(format!("nix-bench security group for run {}", run_id))
            .vpc_id(&vpc_id)
            .tag_specifications(
                TagSpecification::builder()
                    .resource_type(ResourceType::SecurityGroup)
                    .tags(
                        Tag::builder()
                            .key("Name")
                            .value(&sg_name)
                            .build(),
                    )
                    .tags(
                        Tag::builder()
                            .key("nix-bench:run-id")
                            .value(run_id)
                            .build(),
                    )
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

    /// Allocate an Elastic IP address for a benchmark instance
    ///
    /// # Arguments
    /// * `run_id` - The run ID for tagging
    ///
    /// # Returns
    /// A tuple of (allocation_id, public_ip)
    pub async fn allocate_elastic_ip(&self, run_id: &str) -> Result<(String, String)> {
        info!(run_id = %run_id, "Allocating Elastic IP");

        let response = self
            .client
            .allocate_address()
            .domain(aws_sdk_ec2::types::DomainType::Vpc)
            .tag_specifications(
                TagSpecification::builder()
                    .resource_type(ResourceType::ElasticIp)
                    .tags(
                        Tag::builder()
                            .key("Name")
                            .value(format!("nix-bench-{}", run_id))
                            .build(),
                    )
                    .tags(Tag::builder().key("nix-bench:run-id").value(run_id).build())
                    .build(),
            )
            .send()
            .await
            .context("Failed to allocate Elastic IP")?;

        let allocation_id = response
            .allocation_id()
            .context("No allocation ID in response")?
            .to_string();

        let public_ip = response
            .public_ip()
            .context("No public IP in response")?
            .to_string();

        info!(
            allocation_id = %allocation_id,
            public_ip = %public_ip,
            "Allocated Elastic IP"
        );

        Ok((allocation_id, public_ip))
    }

    /// Associate an Elastic IP with an EC2 instance
    ///
    /// # Arguments
    /// * `allocation_id` - The allocation ID of the Elastic IP
    /// * `instance_id` - The instance ID to associate with
    ///
    /// # Returns
    /// The association ID
    pub async fn associate_elastic_ip(
        &self,
        allocation_id: &str,
        instance_id: &str,
    ) -> Result<String> {
        info!(
            allocation_id = %allocation_id,
            instance_id = %instance_id,
            "Associating Elastic IP with instance"
        );

        let response = self
            .client
            .associate_address()
            .allocation_id(allocation_id)
            .instance_id(instance_id)
            .send()
            .await
            .context("Failed to associate Elastic IP")?;

        let association_id = response
            .association_id()
            .context("No association ID in response")?
            .to_string();

        info!(
            association_id = %association_id,
            "Associated Elastic IP with instance"
        );

        Ok(association_id)
    }

    /// Release an Elastic IP address
    ///
    /// # Arguments
    /// * `allocation_id` - The allocation ID of the Elastic IP to release
    pub async fn release_elastic_ip(&self, allocation_id: &str) -> Result<()> {
        info!(allocation_id = %allocation_id, "Releasing Elastic IP");

        self.client
            .release_address()
            .allocation_id(allocation_id)
            .send()
            .await
            .context("Failed to release Elastic IP")?;

        info!(allocation_id = %allocation_id, "Released Elastic IP");

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
