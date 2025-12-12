//! S3 results upload

use crate::config::Config;
use anyhow::{Context, Result};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use serde::Serialize;
use tracing::debug;

/// Result of a single benchmark run
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub run: u32,
    pub duration_secs: f64,
    pub success: bool,
}

/// Complete benchmark results
#[derive(Debug, Serialize)]
struct BenchmarkResults {
    run_id: String,
    instance_type: String,
    system: String,
    attr: String,
    runs: Vec<RunResult>,
}

/// S3 client for uploading results
pub struct S3Client {
    client: Client,
    bucket: String,
    run_id: String,
    instance_type: String,
    system: String,
    attr: String,
}

impl S3Client {
    /// Create a new S3 client
    pub async fn new(config: &Config) -> Result<Self> {
        let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()))
            .load()
            .await;

        let client = Client::new(&aws_config);

        Ok(Self {
            client,
            bucket: config.bucket.clone(),
            run_id: config.run_id.clone(),
            instance_type: config.instance_type.clone(),
            system: config.system.clone(),
            attr: config.attr.clone(),
        })
    }

    /// Upload benchmark results to S3
    pub async fn upload_results(&self, runs: &[RunResult]) -> Result<()> {
        let results = BenchmarkResults {
            run_id: self.run_id.clone(),
            instance_type: self.instance_type.clone(),
            system: self.system.clone(),
            attr: self.attr.clone(),
            runs: runs.to_vec(),
        };

        let json = serde_json::to_string_pretty(&results)
            .context("Failed to serialize results")?;

        let key = format!("{}/{}/results.json", self.run_id, self.instance_type);
        debug!(bucket = %self.bucket, key = %key, "Uploading results");

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(json.into_bytes()))
            .content_type("application/json")
            .send()
            .await
            .context("Failed to upload results to S3")?;

        Ok(())
    }
}
