//! EC2 instance management

use anyhow::{Context, Result};
use aws_sdk_ec2::{
    types::{
        Filter, InstanceStateName, InstanceType, ResourceType, Tag, TagSpecification,
    },
    Client,
};
use tracing::{debug, info};

/// EC2 client for managing benchmark instances
#[allow(dead_code)]
pub struct Ec2Client {
    client: Client,
    region: String,
}

/// Launched instance info
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LaunchedInstance {
    pub instance_id: String,
    pub instance_type: String,
    pub system: String,
    pub public_ip: Option<String>,
}

impl Ec2Client {
    /// Create a new EC2 client
    pub async fn new(region: &str) -> Result<Self> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.to_string()))
            .load()
            .await;

        let client = Client::new(&config);

        Ok(Self {
            client,
            region: region.to_string(),
        })
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
            .filters(
                Filter::builder()
                    .name("state")
                    .values("available")
                    .build(),
            )
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

        let instance_type_enum: InstanceType = instance_type.parse().map_err(|_| {
            anyhow::anyhow!("Invalid instance type: {}", instance_type)
        })?;

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

        let mut request = self
            .client
            .run_instances()
            .image_id(&ami_id)
            .instance_type(instance_type_enum)
            .min_count(1)
            .max_count(1)
            .user_data(&user_data_b64)
            .tag_specifications(
                TagSpecification::builder()
                    .resource_type(ResourceType::Instance)
                    .tags(
                        Tag::builder()
                            .key("Name")
                            .value(format!("nix-bench-{}-{}", run_id, instance_type))
                            .build(),
                    )
                    .tags(
                        Tag::builder()
                            .key("nix-bench:run-id")
                            .value(run_id)
                            .build(),
                    )
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

    /// Wait for an instance to be running and get its public IP
    pub async fn wait_for_running(&self, instance_id: &str) -> Result<Option<String>> {
        info!(instance_id = %instance_id, "Waiting for instance to be running");

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
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                _ => {
                    anyhow::bail!("Instance {} entered unexpected state: {:?}", instance_id, state);
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

    /// Get instance state
    #[allow(dead_code)]
    pub async fn get_instance_state(&self, instance_id: &str) -> Result<Option<InstanceStateName>> {
        let response = self
            .client
            .describe_instances()
            .instance_ids(instance_id)
            .send()
            .await
            .context("Failed to describe instance")?;

        let state = response
            .reservations()
            .first()
            .and_then(|r| r.instances().first())
            .and_then(|i| i.state())
            .and_then(|s| s.name().cloned());

        Ok(state)
    }
}
