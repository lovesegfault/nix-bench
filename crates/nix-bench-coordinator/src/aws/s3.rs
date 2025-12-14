//! S3 bucket and object management

use crate::aws::context::AwsContext;
use anyhow::{Context, Result};
use aws_sdk_s3::{primitives::ByteStream, Client};
use std::path::Path;
use tracing::{debug, info};

/// S3 client for managing benchmark artifacts
pub struct S3Client {
    client: Client,
    region: String,
}

impl S3Client {
    /// Create a new S3 client
    pub async fn new(region: &str) -> Result<Self> {
        let ctx = AwsContext::new(region).await;
        Ok(Self::from_context(&ctx))
    }

    /// Create an S3 client from a pre-loaded AWS context
    pub fn from_context(ctx: &AwsContext) -> Self {
        Self {
            client: ctx.s3_client(),
            region: ctx.region().to_string(),
        }
    }

    /// Create a bucket for this run
    pub async fn create_bucket(&self, bucket_name: &str) -> Result<()> {
        info!(bucket = %bucket_name, region = %self.region, "Creating S3 bucket");

        let location_constraint =
            aws_sdk_s3::types::BucketLocationConstraint::from(self.region.as_str());

        let create_config = aws_sdk_s3::types::CreateBucketConfiguration::builder()
            .location_constraint(location_constraint)
            .build();

        self.client
            .create_bucket()
            .bucket(bucket_name)
            .create_bucket_configuration(create_config)
            .send()
            .await
            .context("Failed to create bucket")?;

        Ok(())
    }

    /// Upload a file to S3
    pub async fn upload_file(&self, bucket: &str, key: &str, path: &Path) -> Result<()> {
        debug!(bucket = %bucket, key = %key, path = %path.display(), "Uploading file");

        let body = ByteStream::from_path(path)
            .await
            .context("Failed to read file")?;

        self.client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(body)
            .send()
            .await
            .context("Failed to upload file")?;

        Ok(())
    }

    /// Upload bytes to S3
    pub async fn upload_bytes(
        &self,
        bucket: &str,
        key: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        debug!(bucket = %bucket, key = %key, size = data.len(), "Uploading bytes");

        self.client
            .put_object()
            .bucket(bucket)
            .key(key)
            .body(ByteStream::from(data))
            .content_type(content_type)
            .send()
            .await
            .context("Failed to upload bytes")?;

        Ok(())
    }

    /// Delete a bucket and all its objects
    pub async fn delete_bucket(&self, bucket: &str) -> Result<()> {
        info!(bucket = %bucket, "Deleting bucket and contents");

        // List and delete all objects
        let mut continuation_token = None;
        loop {
            let mut request = self.client.list_objects_v2().bucket(bucket);

            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }

            let response = request.send().await.context("Failed to list objects")?;

            for object in response.contents() {
                if let Some(key) = object.key() {
                    debug!(key = %key, "Deleting object");
                    self.client
                        .delete_object()
                        .bucket(bucket)
                        .key(key)
                        .send()
                        .await
                        .context("Failed to delete object")?;
                }
            }

            if response.is_truncated() == Some(true) {
                continuation_token = response.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        // Delete the bucket itself
        self.client
            .delete_bucket()
            .bucket(bucket)
            .send()
            .await
            .context("Failed to delete bucket")?;

        Ok(())
    }
}
