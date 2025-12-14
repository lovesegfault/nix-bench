//! Shared AWS configuration context
//!
//! Provides `AwsContext` for loading AWS SDK configuration once and
//! creating multiple service clients from the same config.

use aws_config::{BehaviorVersion, Region, SdkConfig};
use std::sync::Arc;

/// Shared AWS configuration context for creating service clients.
///
/// This struct holds a loaded AWS SDK config and provides methods
/// to create service clients without re-loading configuration.
///
/// # Example
/// ```ignore
/// let aws = AwsContext::new("us-east-2").await;
///
/// // Create multiple clients from the same config
/// let ec2 = Ec2Client::from_context(&aws);
/// let s3 = S3Client::from_context(&aws);
/// let iam = IamClient::from_context(&aws);
/// ```
#[derive(Clone)]
pub struct AwsContext {
    config: Arc<SdkConfig>,
    region: String,
}

impl AwsContext {
    /// Load AWS configuration for the specified region.
    ///
    /// This loads credentials, region configuration, and other AWS SDK
    /// settings from the environment, config files, and IAM roles.
    pub async fn new(region: &str) -> Self {
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region.to_string()))
            .load()
            .await;

        Self {
            config: Arc::new(config),
            region: region.to_string(),
        }
    }

    /// Get the underlying SDK config for direct client construction.
    pub fn sdk_config(&self) -> &SdkConfig {
        &self.config
    }

    /// Get the region string.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Create an EC2 client from this context.
    pub fn ec2_client(&self) -> aws_sdk_ec2::Client {
        aws_sdk_ec2::Client::new(self.sdk_config())
    }

    /// Create an IAM client from this context.
    pub fn iam_client(&self) -> aws_sdk_iam::Client {
        aws_sdk_iam::Client::new(self.sdk_config())
    }

    /// Create an STS client from this context.
    pub fn sts_client(&self) -> aws_sdk_sts::Client {
        aws_sdk_sts::Client::new(self.sdk_config())
    }

    /// Create an S3 client from this context.
    pub fn s3_client(&self) -> aws_sdk_s3::Client {
        aws_sdk_s3::Client::new(self.sdk_config())
    }
}

impl std::fmt::Debug for AwsContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsContext")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require AWS credentials and are marked as integration tests
    // They are skipped in regular test runs

    #[tokio::test]
    #[ignore = "requires AWS credentials"]
    async fn test_context_creation() {
        let ctx = AwsContext::new("us-east-2").await;
        assert_eq!(ctx.region(), "us-east-2");
    }

    #[tokio::test]
    #[ignore = "requires AWS credentials"]
    async fn test_context_clone() {
        let ctx1 = AwsContext::new("us-east-2").await;
        let ctx2 = ctx1.clone();

        // Both should point to the same Arc'd config
        assert_eq!(ctx1.region(), ctx2.region());
    }
}
