//! EC2 instance management

mod instance;
mod security_group;
mod types;

pub use types::{LaunchInstanceConfig, LaunchedInstance};

use crate::aws::context::AwsContext;
use anyhow::{Context, Result};
use aws_sdk_ec2::{Client, types::Filter};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tracing::debug;

/// EC2 client for managing benchmark instances
pub struct Ec2Client {
    pub(crate) client: Client,
    /// Cache of AMI IDs by architecture (only 2: x86_64, arm64)
    ami_cache: Mutex<HashMap<String, String>>,
}

impl Ec2Client {
    pub fn from_context(ctx: &AwsContext) -> Self {
        Self {
            client: ctx.ec2_client(),
            ami_cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn new(region: &str) -> Result<Self> {
        let ctx = AwsContext::new(region).await;
        Ok(Self::from_context(&ctx))
    }
}

impl Ec2Client {
    /// Get the latest AL2023 AMI for the given architecture (cached)
    pub async fn get_al2023_ami(&self, arch: &str) -> Result<String> {
        let arch_filter = if arch == "aarch64-linux" {
            "arm64"
        } else {
            "x86_64"
        };

        // Check cache first (hold lock briefly)
        {
            let cache = self.ami_cache.lock().unwrap();
            if let Some(ami) = cache.get(arch_filter) {
                debug!(ami = %ami, arch = %arch, "Using cached AL2023 AMI");
                return Ok(ami.clone());
            }
        }

        // Not cached, fetch from AWS (outside lock)
        let ami = self.fetch_al2023_ami(arch_filter).await?;

        // Use entry API to avoid TOCTOU race: if another task raced and
        // already inserted while we were fetching, use the existing value.
        let ami = {
            let mut cache = self.ami_cache.lock().unwrap();
            cache.entry(arch_filter.to_string()).or_insert(ami).clone()
        };

        debug!(ami = %ami, arch = %arch, "Found and cached AL2023 AMI");
        Ok(ami)
    }

    /// Validate that all instance types exist in AWS
    ///
    /// Returns Ok(()) if all instance types are valid.
    /// Returns Err with InvalidInstanceType if any are invalid.
    pub async fn validate_instance_types(&self, instance_types: &[String]) -> Result<()> {
        use crate::aws::error::AwsError;

        if instance_types.is_empty() {
            return Ok(());
        }

        let response = self
            .client
            .describe_instance_types()
            .set_instance_types(Some(
                instance_types
                    .iter()
                    .map(|s| aws_sdk_ec2::types::InstanceType::from(s.as_str()))
                    .collect(),
            ))
            .send()
            .await;

        match response {
            Ok(output) => {
                // Check which instance types were actually found
                let found: std::collections::HashSet<_> = output
                    .instance_types()
                    .iter()
                    .filter_map(|it| it.instance_type())
                    .map(|t| t.as_str().to_string())
                    .collect();

                let invalid: Vec<_> = instance_types
                    .iter()
                    .filter(|t| !found.contains(*t))
                    .cloned()
                    .collect();

                if invalid.is_empty() {
                    debug!(count = instance_types.len(), "All instance types validated");
                    Ok(())
                } else {
                    Err(AwsError::InvalidInstanceType {
                        invalid_types: invalid,
                    }
                    .into())
                }
            }
            Err(e) => {
                // AWS returns InvalidInstanceType error if any type is invalid
                // Message format: "The following supplied instance types do not exist: [c8i.48xlarage, foo.bar]"
                let err_str = format!("{:?}", e);
                if err_str.contains("InvalidInstanceType") {
                    // Extract invalid types from the error message
                    let invalid = extract_invalid_types(&err_str);
                    Err(AwsError::InvalidInstanceType {
                        invalid_types: invalid,
                    }
                    .into())
                } else {
                    Err(anyhow::anyhow!("Failed to validate instance types: {}", e))
                }
            }
        }
    }

    /// Fetch the latest AL2023 AMI from AWS (internal, no caching)
    async fn fetch_al2023_ami(&self, arch_filter: &str) -> Result<String> {
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

        Ok(ami.to_string())
    }
}

/// Extract invalid instance types from AWS error message.
///
/// AWS returns messages like:
/// "The following supplied instance types do not exist: [c8i.48xlarage, foo.bar]"
fn extract_invalid_types(error_str: &str) -> Vec<String> {
    // Look for the bracketed list in the error message
    if let Some(start) = error_str.find("[") {
        if let Some(end) = error_str[start..].find("]") {
            let bracket_content = &error_str[start + 1..start + end];
            return bracket_content
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }
    // Fallback: return empty vec (error message will still show the raw error)
    Vec::new()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_invalid_types_single() {
        let err = "The following supplied instance types do not exist: [c8i.48xlarage]";
        assert_eq!(extract_invalid_types(err), vec!["c8i.48xlarage"]);
    }

    #[test]
    fn test_extract_invalid_types_multiple() {
        let err = "The following supplied instance types do not exist: [c8i.48xlarage, foo.bar]";
        assert_eq!(extract_invalid_types(err), vec!["c8i.48xlarage", "foo.bar"]);
    }

    #[test]
    fn test_extract_invalid_types_no_brackets() {
        let err = "Some other error message";
        assert_eq!(extract_invalid_types(err), Vec::<String>::new());
    }
}
