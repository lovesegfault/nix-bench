//! EC2 instance management

mod instance;
mod operations;
mod security_group;
mod types;

pub use operations::Ec2Operations;
pub use types::{LaunchInstanceConfig, LaunchedInstance};

#[cfg(test)]
pub use operations::MockEc2Operations;

use crate::aws::context::AwsContext;
use anyhow::{Context, Result};
use aws_sdk_ec2::{types::Filter, Client};
use std::time::Duration;
use tracing::debug;

/// EC2 client for managing benchmark instances
pub struct Ec2Client {
    pub(crate) client: Client,
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
