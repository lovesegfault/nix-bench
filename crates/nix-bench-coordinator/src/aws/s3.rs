//! S3 bucket and object management

use crate::aws::context::AwsContext;
use crate::aws::error::classify_aws_error;
use anyhow::{Context, Result};
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::{primitives::ByteStream, Client};
use chrono::Utc;
use nix_bench_common::tags::{self, TAG_CREATED_AT, TAG_RUN_ID, TAG_STATUS, TAG_TOOL, TAG_TOOL_VALUE};
use std::future::Future;
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
    ///
    /// This is idempotent - if the bucket already exists and is owned by you,
    /// it succeeds without error.
    pub async fn create_bucket(&self, bucket_name: &str) -> Result<()> {
        info!(bucket = %bucket_name, region = %self.region, "Creating S3 bucket");

        let location_constraint =
            aws_sdk_s3::types::BucketLocationConstraint::from(self.region.as_str());

        let create_config = aws_sdk_s3::types::CreateBucketConfiguration::builder()
            .location_constraint(location_constraint)
            .build();

        let result = self
            .client
            .create_bucket()
            .bucket(bucket_name)
            .create_bucket_configuration(create_config)
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                // BucketAlreadyOwnedByYou means we already own it - that's fine
                if e.code() == Some("BucketAlreadyOwnedByYou") {
                    debug!(bucket = %bucket_name, "Bucket already exists and is owned by us");
                    Ok(())
                } else {
                    Err(e).context("Failed to create bucket")
                }
            }
        }
    }

    /// Apply standard nix-bench tags to a bucket
    ///
    /// S3 bucket tagging requires a separate API call after bucket creation.
    /// This should be called immediately after `create_bucket`.
    pub async fn tag_bucket(&self, bucket_name: &str, run_id: &str) -> Result<()> {
        debug!(bucket = %bucket_name, run_id = %run_id, "Tagging S3 bucket");

        let created_at = tags::format_created_at(Utc::now());

        let tagging = aws_sdk_s3::types::Tagging::builder()
            .tag_set(
                aws_sdk_s3::types::Tag::builder()
                    .key(TAG_TOOL)
                    .value(TAG_TOOL_VALUE)
                    .build()?,
            )
            .tag_set(
                aws_sdk_s3::types::Tag::builder()
                    .key(TAG_RUN_ID)
                    .value(run_id)
                    .build()?,
            )
            .tag_set(
                aws_sdk_s3::types::Tag::builder()
                    .key(TAG_CREATED_AT)
                    .value(&created_at)
                    .build()?,
            )
            .tag_set(
                aws_sdk_s3::types::Tag::builder()
                    .key(TAG_STATUS)
                    .value(tags::status::CREATING)
                    .build()?,
            )
            .build()?;

        self.client
            .put_bucket_tagging()
            .bucket(bucket_name)
            .tagging(tagging)
            .send()
            .await
            .context("Failed to tag bucket")?;

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
    ///
    /// Returns Ok(()) if the bucket was deleted or if it doesn't exist (idempotent for cleanup).
    pub async fn delete_bucket(&self, bucket: &str) -> Result<()> {
        info!(bucket = %bucket, "Deleting bucket and contents");

        // List and delete all objects
        let mut continuation_token = None;
        loop {
            let mut request = self.client.list_objects_v2().bucket(bucket);

            if let Some(token) = &continuation_token {
                request = request.continuation_token(token);
            }

            match request.send().await {
                Ok(response) => {
                    for object in response.contents() {
                        if let Some(key) = object.key() {
                            debug!(key = %key, "Deleting object");
                            // Ignore not-found errors for individual objects
                            if let Err(sdk_error) = self
                                .client
                                .delete_object()
                                .bucket(bucket)
                                .key(key)
                                .send()
                                .await
                            {
                                if !classify_aws_error(sdk_error.code(), sdk_error.message())
                                    .is_not_found()
                                {
                                    return Err(anyhow::Error::from(sdk_error)
                                        .context("Failed to delete object"));
                                }
                            }
                        }
                    }

                    if response.is_truncated() == Some(true) {
                        continuation_token =
                            response.next_continuation_token().map(|s| s.to_string());
                    } else {
                        break;
                    }
                }
                Err(sdk_error) => {
                    // If bucket doesn't exist, we're done
                    if classify_aws_error(sdk_error.code(), sdk_error.message()).is_not_found() {
                        debug!(bucket = %bucket, "Bucket already deleted or doesn't exist");
                        return Ok(());
                    }
                    return Err(anyhow::Error::from(sdk_error).context("Failed to list objects"));
                }
            }
        }

        // Delete the bucket itself
        match self.client.delete_bucket().bucket(bucket).send().await {
            Ok(_) => {
                info!(bucket = %bucket, "Bucket deleted");
                Ok(())
            }
            Err(sdk_error) => {
                if classify_aws_error(sdk_error.code(), sdk_error.message()).is_not_found() {
                    debug!(bucket = %bucket, "Bucket already deleted or doesn't exist");
                    Ok(())
                } else {
                    Err(anyhow::Error::from(sdk_error).context("Failed to delete bucket"))
                }
            }
        }
    }
}

/// Trait for S3 operations that can be mocked in tests.
pub trait S3Operations: Send + Sync {
    /// Create a bucket
    fn create_bucket(&self, bucket_name: &str) -> impl Future<Output = Result<()>> + Send;

    /// Apply standard nix-bench tags to a bucket
    fn tag_bucket(
        &self,
        bucket_name: &str,
        run_id: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Upload a file to S3
    fn upload_file(
        &self,
        bucket: &str,
        key: &str,
        path: &Path,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Upload bytes to S3
    fn upload_bytes(
        &self,
        bucket: &str,
        key: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Delete a bucket and all its objects
    fn delete_bucket(&self, bucket: &str) -> impl Future<Output = Result<()>> + Send;
}

impl S3Operations for S3Client {
    async fn create_bucket(&self, bucket_name: &str) -> Result<()> {
        S3Client::create_bucket(self, bucket_name).await
    }

    async fn tag_bucket(&self, bucket_name: &str, run_id: &str) -> Result<()> {
        S3Client::tag_bucket(self, bucket_name, run_id).await
    }

    async fn upload_file(&self, bucket: &str, key: &str, path: &std::path::Path) -> Result<()> {
        S3Client::upload_file(self, bucket, key, path).await
    }

    async fn upload_bytes(
        &self,
        bucket: &str,
        key: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<()> {
        S3Client::upload_bytes(self, bucket, key, data, content_type).await
    }

    async fn delete_bucket(&self, bucket: &str) -> Result<()> {
        S3Client::delete_bucket(self, bucket).await
    }
}
